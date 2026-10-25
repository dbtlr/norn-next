//! Shared document loading for first-index construction.
use super::super::*;
use crate::derivation::plan_base_document;

#[derive(Clone, Copy, PartialEq)]
pub(in crate::production) enum Depth {
    Rich,
    Metadata,
}

pub(in crate::production) fn load_fact(
    pending: &mut Pending<'_>,
    fact: norn_fs::WalkFact,
    depth: Depth,
    files: bool,
) -> Result<Option<String>, JobFailure> {
    match fact {
        norn_fs::WalkFact::Skipped(skip) => {
            if files {
                pending.passed_over(skip.path().as_path(), skip.reason());
            } else if !skip.reason().stands() {
                pending.account.withheld.absorb(skip.path().as_path());
            }
        }
        norn_fs::WalkFact::File(file) if !is_markdown(file.path().as_path()) => {
            if files
                && let Ok(path) = document_path(file.path().as_path())
                && path.is_vault_file()
            {
                pending.push(Change::FileBirth { path });
            }
        }
        norn_fs::WalkFact::File(file) => {
            let path = file.path().as_path().to_owned();
            let Some(spelling) = path.to_str() else {
                if let Err(quarantine) = document_path(&path) {
                    pending.quarantine(&path, quarantine);
                }
                return Ok(None);
            };
            let Some(read) = file.read_optional().map_err(effect)? else {
                pending.withhold_entry(&path);
                return Ok(None);
            };
            pending.account.bytes_read += read.bytes().len() as u64;
            let stored = match document_path(&path) {
                Ok(path) => pending
                    .store
                    .begin_request()
                    .stored_document(&path)
                    .map_err(store_effect)?
                    .map(|row| row.path),
                Err(_) => None,
            };
            let hash = read.content_hash().to_string();
            count_document_derived();
            let mut plan = if depth != Depth::Rich {
                plan_base_document(
                    &path,
                    spelling,
                    read.bytes(),
                    hash.clone(),
                    stored.as_ref(),
                    &pending.declared.model,
                    pending.store.path_order().glob_case(),
                )
            } else {
                plan_document(
                    &path,
                    spelling,
                    read.bytes(),
                    hash.clone(),
                    stored.as_ref(),
                    &pending.declared.model,
                    pending.store.path_order().glob_case(),
                )
            };
            if depth == Depth::Metadata {
                plan.findings.retain(|finding| {
                    matches!(
                        finding.cause,
                        crate::derivation::Cause::Undecodable(_)
                            | crate::derivation::Cause::UnreadBlock(_)
                    )
                });
            }
            pending.record_plan(plan, hash.clone());
            return Ok(Some(hash));
        }
    }
    Ok(None)
}
