//! The canonical names, content and structure passes; disposable early sources stay separate.
use super::*;
use crate::derivation::{plan_content_document, plan_structure_document};
use norn_store::{ContentWrite, Span, Step};
use std::os::unix::ffi::OsStringExt;

/// Run the canonical document steps with one walk account, closing it only once
/// content has settled every provisional name and every refused spelling.
pub(super) fn documents(
    ops: &ProductionEntryOps,
    store: &mut Store,
    root: &Path,
    exclusions: &[PathBuf],
    progress: &Healing<'_, ProductionAttachment>,
    checkpoint: &mut dyn FnMut(),
) -> Result<SkippedLinks, JobFailure> {
    let policy = ops.policy;
    let walk = walk(root, exclusions).map_err(effect)?;
    let sensitivity = walk.case_sensitivity();
    let mut account = Account::default();
    merge_walk_checkpoint(
        store,
        root,
        exclusions,
        walk,
        sensitivity,
        HealScope::Vault,
        policy,
        progress,
        &mut account,
        Some(checkpoint),
    )?;
    store.finish_step(Step::Names).map_err(store_effect)?;
    boundary(ops, Step::Names);
    let source = norn_fs::Vault::open(root, exclusions).map_err(effect)?;
    loop {
        let mut group = store.step_group().map_err(store_effect)?;
        let mut count = 0;
        let mut pending = Pending::new(
            group.store(),
            policy.changeset_size,
            root,
            exclusions,
            &mut account,
        )?;
        let mut previous: Option<DocumentPath> = None;
        while count < 2048 {
            let mut request = pending.store.begin_request();
            let result = request.content_page(policy.changeset_size.min(2048 - count));
            let steps = request.read_steps();
            count_increment_work(&request.finish(), steps);
            let paths = result.map_err(store_effect)?;
            if paths.is_empty() {
                break;
            }
            // Every chunk takes each of its paths out of the queue. One that
            // pages the same first path again made no progress, and the build
            // fails rather than spin: its mark makes the next open rebuild.
            if previous.as_ref() == paths.first() {
                return Err(JobFailure::Environmental(format!(
                    "the content step could not settle `{}`",
                    paths[0].as_str()
                )));
            }
            previous = paths.first().cloned();
            count += paths.len();
            content_chunk(ops, &mut pending, &source, &paths)?;
            pending.flush()?;
            checkpoint();
        }
        drop(pending);
        group.commit().map_err(store_effect)?;
        if count == 0 {
            break;
        }
    }
    // Spelling findings must see the content step's settled occupancy. An
    // undecodable provisional document no longer withholds a refused spelling.
    let mut pending = Pending::new(store, policy.changeset_size, root, exclusions, &mut account)?;
    loop {
        let paths = pending
            .store
            .begin_request()
            .construction_spellings(policy.changeset_size)
            .map_err(store_effect)?;
        if paths.is_empty() {
            break;
        }
        for bytes in paths {
            let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
            if let Err(quarantine) = document_path(&path) {
                pending.quarantine(&path, quarantine)?;
            }
        }
        // Findings alone use an ordinary transaction once the groups are closed.
        let findings = pending.recordable()?;
        let mut request = pending.store.begin_request();
        request
            .apply_increment(
                IncrementProvenance::Derived,
                [],
                &findings,
                pending.declared.model.content_model(),
            )
            .map_err(store_effect)?;
        let steps = request.read_steps();
        count_increment_work(&request.finish(), steps);
    }
    drop(pending);
    close_job(store, root, exclusions, policy, &mut account)?;
    store.finish_step(Step::Content).map_err(store_effect)?;
    boundary(ops, Step::Content);
    let declared = Declaration::read(store)?;
    loop {
        let mut group = store.step_group().map_err(store_effect)?;
        let mut count = 0;
        while count < 2048 {
            let mut request = group.begin_request();
            let result = request.structure_page(policy.changeset_size.min(2048 - count));
            let steps = request.read_steps();
            count_increment_work(&request.finish(), steps);
            let rows = result.map_err(store_effect)?;
            if rows.is_empty() {
                break;
            }
            count += rows.len();
            structure_chunk(group.store(), rows, &declared.model)?;
            checkpoint();
        }
        group.commit().map_err(store_effect)?;
        if count == 0 {
            break;
        }
    }
    store.finish_step(Step::Structure).map_err(store_effect)?;
    boundary(ops, Step::Structure);
    Ok(account.links)
}

/// Derive an arbitrary set of at most one host chunk, with no held-row input.
/// NORN-468 (pull-ahead) and NORN-469 (requeue) will select single paths here;
/// today the vault pass alone supplies the stage-1 queue.
pub(in crate::production) fn content_chunk(
    ops: &ProductionEntryOps,
    pending: &mut Pending<'_>,
    source: &norn_fs::Vault,
    paths: &[DocumentPath],
) -> Result<(), JobFailure> {
    #[cfg(feature = "induced-failure")]
    let _timing = norn_store::profiling::span("host.step.content");
    for path in paths {
        // An earlier open may have marked this path's whole directory range.
        if !pending
            .store
            .begin_request()
            .content_owed(path)
            .map_err(store_effect)?
        {
            continue;
        }
        let fact = source.fact(Path::new(path.as_str())).map_err(effect)?;
        let opened = match fact {
            norn_fs::WalkFact::File(file) => {
                ops.evidence.count_build_read();
                Some(file.read_optional().map_err(effect)?)
            }
            norn_fs::WalkFact::Skipped(skip) => {
                pending
                    .store
                    .begin_request()
                    .withdraw_content(path)
                    .map_err(store_effect)?;
                pending.passed_over(skip.path().as_path(), skip.reason());
                pending.withhold_entry(Path::new(path.as_str()));
                None
            }
            norn_fs::WalkFact::Unreadable(unreadable) => {
                // A name step 1 enumerated that is now a directory the walk
                // cannot list is no document: its provisional row owns no
                // death and is withdrawn, and the directory is marked at it.
                if unreadable.kind() == norn_fs::UnreadKind::Directory
                    && unreadable.path().as_path() == Path::new(path.as_str())
                {
                    pending
                        .store
                        .begin_request()
                        .withdraw_content(path)
                        .map_err(store_effect)?;
                }
                pending.flush()?;
                pending.unreadable_fact(&unreadable, None)?;
                pending.flush()?;
                None
            }
        };
        match opened {
            Some(norn_fs::Opened::Read(read)) => {
                pending.account.bytes_read += read.bytes().len() as u64;
                count_document_derived();
                let hash = read.content_hash().to_string();
                let (mut plan, origin) = plan_content_document(
                    Path::new(path.as_str()),
                    path.as_str(),
                    read.bytes(),
                    hash.clone(),
                    None,
                    &pending.declared.model,
                    pending.store.path_order().glob_case(),
                );
                if let Some(Change::Upsert(facts)) = plan.change.take() {
                    let origin = origin.expect("a readable document has a body origin");
                    pending.content.push(ContentWrite {
                        facts,
                        origin: Span {
                            line: origin.line as u64,
                            column: origin.column as u64,
                            byte_offset: origin.byte_offset as u64,
                        },
                    });
                } else {
                    pending
                        .store
                        .begin_request()
                        .withdraw_content(path)
                        .map_err(store_effect)?;
                }
                pending.record_plan(plan, hash);
            }
            Some(norn_fs::Opened::Nothing) => {
                pending
                    .store
                    .begin_request()
                    .withdraw_content(path)
                    .map_err(store_effect)?;
                pending.withhold_entry(Path::new(path.as_str()));
            }
            Some(norn_fs::Opened::UnreadableFolder { folder, cause, .. }) => {
                pending.flush()?;
                pending.unlistable(&folder, &cause)?;
                pending.flush()?;
            }
            Some(norn_fs::Opened::Unreadable { operation, cause }) => {
                pending.flush()?;
                pending.unreadable(Path::new(path.as_str()), operation, &cause)?;
                pending.flush()?;
            }
            None => {}
        }
    }
    Ok(())
}

/// Scan stored bytes one document at a time, then release that scan before the
/// next. NORN-468 pull-ahead and NORN-469 requeue will supply arbitrary single
/// documents to this same seam; they have no caller in this layer yet.
fn structure_chunk(
    store: &mut Store,
    mut rows: Vec<norn_store::StructureInput>,
    declared: &Declared,
) -> Result<(), JobFailure> {
    #[cfg(feature = "induced-failure")]
    let _timing = norn_store::profiling::span("host.step.structure");
    // Write and release each plan before scanning the next body. Holding an
    // entire chunk of scans or mapped facts would make dense bodies multiply.
    let mut account = Account::default();
    let mut pending = Pending::new(store, 1, Path::new(""), &[], &mut account)?;
    for input in &mut rows {
        let mut request = pending.store.begin_request();
        request.load_structure_tags(input).map_err(store_effect)?;
        let steps = request.read_steps();
        count_increment_work(&request.finish(), steps);
        let origin = norn_text::SourceSpan {
            line: input.origin.line as usize,
            column: input.origin.column as usize,
            byte_offset: input.origin.byte_offset as usize,
        };
        for finding in plan_structure_document(&mut input.facts, origin, declared) {
            pending.file(finding, None);
        }
        let findings = pending.recordable()?;
        let mut request = pending.store.begin_request();
        request
            .write_structure(
                std::slice::from_ref(input),
                &findings,
                declared.content_model(),
            )
            .map_err(store_effect)?;
        let steps = request.read_steps();
        count_increment_work(&request.finish(), steps);
        input.facts.links = Vec::new();
        input.facts.tags = Vec::new();
        input.facts.headings = Vec::new();
        input.facts.blocks = Vec::new();
    }
    Ok(())
}

/// Publish a completed step only after its last group commits.
fn boundary(ops: &ProductionEntryOps, step: Step) {
    #[cfg(feature = "induced-failure")]
    ops.loading_boundary(LoadingBoundary::StepComplete(step));
    #[cfg(not(feature = "induced-failure"))]
    let _ = (ops, step);
}
