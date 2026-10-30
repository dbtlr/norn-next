//! The canonical names, content and structure passes, each published as it
//! ends, with requested Get targets pulled ahead at their chunk boundaries.
use super::*;
use crate::derivation::{plan_content_document, plan_structure_document};
use norn_store::{AmbiguityIgnore, ContentWrite, Span, Step};
use std::os::unix::ffi::OsStringExt;

/// Run the canonical document steps with one walk account, closing it only once
/// content has settled every provisional name and every refused spelling.
///
/// Requested Get targets are drained ([`drain`]) as each step's first group
/// opens — the names boundary for content — and at every chunk boundary
/// inside the open group, steps 2 and 3 only.
pub(super) fn documents(
    ops: &ProductionEntryOps,
    store: &mut Store,
    root: &Path,
    exclusions: &[PathBuf],
    progress: &Healing<'_, ProductionAttachment>,
    publisher: &Publisher<'_>,
    subscription: Option<&Subscription>,
) -> Result<SkippedLinks, JobFailure> {
    let mut checkpoint = || {
        #[cfg(feature = "induced-failure")]
        ops.loading_boundary(LoadingBoundary::CanonicalPage);
    };
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
        Some(&mut checkpoint),
    )?;
    requeue(
        ops,
        store,
        root,
        exclusions,
        progress,
        publisher,
        subscription,
        false,
        &mut account,
    )?;
    store.finish_step(Step::Names).map_err(store_effect)?;
    boundary(ops, publisher, Step::Names);
    let source = norn_fs::Vault::open(root, exclusions).map_err(effect)?;
    let commits = store.construction_group_commits();
    let mut opening = true;
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
        if std::mem::take(&mut opening) {
            pending.flush()?;
            requeue(
                ops,
                pending.store,
                root,
                exclusions,
                progress,
                publisher,
                subscription,
                true,
                pending.account,
            )?;
            drain(ops, publisher, &mut Scope::Content(&mut pending, &source))?;
        }
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
            requeue(
                ops,
                pending.store,
                root,
                exclusions,
                progress,
                publisher,
                subscription,
                true,
                pending.account,
            )?;
            drain(ops, publisher, &mut Scope::Content(&mut pending, &source))?;
        }
        pending.flush()?;
        requeue(
            ops,
            pending.store,
            root,
            exclusions,
            progress,
            publisher,
            subscription,
            true,
            pending.account,
        )?;
        drop(pending);
        group.commit().map_err(store_effect)?;
        if count == 0 {
            break;
        }
    }
    ops.evidence
        .count_step_commits(Step::Content, store.construction_group_commits() - commits);
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
    requeue(
        ops,
        store,
        root,
        exclusions,
        progress,
        publisher,
        subscription,
        false,
        &mut account,
    )?;
    store.finish_step(Step::Content).map_err(store_effect)?;
    boundary(ops, publisher, Step::Content);
    let declared = Declaration::read(store)?;
    let commits = store.construction_group_commits();
    let mut opening = true;
    loop {
        let mut group = store.step_group().map_err(store_effect)?;
        let mut count = 0;
        if std::mem::take(&mut opening) {
            requeue(
                ops,
                group.store(),
                root,
                exclusions,
                progress,
                publisher,
                subscription,
                true,
                &mut account,
            )?;
            drain(
                ops,
                publisher,
                &mut Scope::Structure(group.store(), &declared.model),
            )?;
        }
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
            requeue(
                ops,
                group.store(),
                root,
                exclusions,
                progress,
                publisher,
                subscription,
                true,
                &mut account,
            )?;
            drain(
                ops,
                publisher,
                &mut Scope::Structure(group.store(), &declared.model),
            )?;
        }
        requeue(
            ops,
            group.store(),
            root,
            exclusions,
            progress,
            publisher,
            subscription,
            true,
            &mut account,
        )?;
        group.commit().map_err(store_effect)?;
        if count == 0 {
            break;
        }
    }
    ops.evidence.count_step_commits(
        Step::Structure,
        store.construction_group_commits() - commits,
    );
    requeue(
        ops,
        store,
        root,
        exclusions,
        progress,
        publisher,
        subscription,
        false,
        &mut account,
    )?;
    store.finish_step(Step::Structure).map_err(store_effect)?;
    boundary(ops, publisher, Step::Structure);
    Ok(account.links)
}

/// Take accepted roots ahead of the ordinary queue, finish their owed steps,
/// commit, then publish their derivation. Nothing calls this from FullText on.
#[allow(clippy::too_many_arguments)]
pub(super) fn requeue(
    ops: &ProductionEntryOps,
    store: &mut Store,
    root: &Path,
    exclusions: &[PathBuf],
    progress: &Healing<'_, ProductionAttachment>,
    publisher: &Publisher<'_>,
    subscription: Option<&Subscription>,
    grouped: bool,
    walk: &mut Account,
) -> Result<(), JobFailure> {
    let Some(subscription) = subscription else {
        return Ok(());
    };
    if publisher
        .authority
        .as_ref()
        .is_none_or(|authority| !authority.admits())
    {
        return Ok(());
    }
    while let Some(taken) = subscription.take_build_roots() {
        if taken.roots.is_empty() {
            break;
        }
        if grouped || store.construction_step() == Some(Step::Names) {
            requeue_taken(
                ops, store, root, exclusions, progress, publisher, &taken, walk,
            )?;
        } else {
            let mut group = store.step_group().map_err(store_effect)?;
            requeue_taken(
                ops,
                group.store(),
                root,
                exclusions,
                progress,
                publisher,
                &taken,
                walk,
            )?;
            group.commit().map_err(store_effect)?;
        }
    }
    Ok(())
}

/// Finish one taken set, using the caller's document-step group when needed.
///
/// The requeue's walk and its content close jobs of their own, and the step's
/// walk account (`walk`) takes what they filed, so the step's own job end
/// does not prune a finding the requeue filed again.
#[allow(clippy::too_many_arguments)]
fn requeue_taken(
    ops: &ProductionEntryOps,
    store: &mut Store,
    root: &Path,
    exclusions: &[PathBuf],
    progress: &Healing<'_, ProductionAttachment>,
    publisher: &Publisher<'_>,
    taken: &norn_fs::watch::TakenRoots,
    walk: &mut Account,
) -> Result<(), JobFailure> {
    #[cfg(feature = "induced-failure")]
    ops.loading_boundary(LoadingBoundary::RequeueStarted);
    let (result, steps) = store.measure_read_steps(|store| {
        let began = std::time::Instant::now();
        let commits = store.construction_write_commits();
        if store.construction_step() != Some(Step::Names) {
            store.commit_step_group().map_err(store_effect)?;
        }
        walk.absorb_requeue(
            scoped_increment_closed(store, root, &taken.roots, ops.policy, progress, exclusions)?,
            store.path_order(),
        );
        let mut documents = 0;
        if store.construction_step() != Some(Step::Names) {
            let source = norn_fs::Vault::open(root, exclusions).map_err(effect)?;
            let mut account = Account::default();
            loop {
                let paths = {
                    let mut request = store.begin_request();
                    request
                        .requeued_page(ops.policy.changeset_size)
                        .map_err(store_effect)?
                };
                if paths.is_empty() {
                    break;
                }
                store
                    .begin_request()
                    .dequeue_requeued(&paths)
                    .map_err(store_effect)?;
                let stage = store.construction_step();
                let mut pending = Pending::new(
                    store,
                    ops.policy.changeset_size,
                    root,
                    exclusions,
                    &mut account,
                )?;
                let content: Vec<_> = paths
                    .iter()
                    .filter_map(
                        |path| match pending.store.begin_request().content_owed(path) {
                            Ok(true) => Some(Ok(path.clone())),
                            Ok(false) => None,
                            Err(error) => Some(Err(store_effect(error))),
                        },
                    )
                    .collect::<Result<_, _>>()?;
                documents += content.len() as u64;
                content_chunk(ops, &mut pending, &source, &content)?;
                pending.flush()?;
                // Each reset ended every finding at its place, including the
                // spellings that render there. Where content leaves no row,
                // those spellings stand again, as the content step's spelling
                // pass reads them once occupancy settles.
                for path in &content {
                    let stands = pending
                        .store
                        .begin_request()
                        .stored_document(path)
                        .map_err(store_effect)?
                        .is_some();
                    if !stands {
                        pending.refile_reset_place(&source, path)?;
                    }
                }
                pending.flush()?;
                pending.store.commit_step_group().map_err(store_effect)?;
                if matches!(stage, Some(Step::Structure | Step::Relations)) {
                    for path in paths {
                        let row = pending
                            .store
                            .begin_request()
                            .structure_document(&path)
                            .map_err(store_effect)?;
                        if let Some(row) = row {
                            structure_chunk(pending.store, vec![row], &pending.declared.model)?;
                        }
                    }
                }
                pending.store.commit_step_group().map_err(store_effect)?;
            }
            close_job(store, root, exclusions, ops.policy, &mut account)?;
            walk.absorb_requeue(account, store.path_order());
        }
        if store.construction_step() != Some(Step::Names) {
            store.commit_step_group().map_err(store_effect)?;
        }
        if store.construction_step() == Some(Step::Relations) {
            let declared = Declaration::read(store)?;
            let mut request = store.begin_request();
            let links = request
                .rejudge_reach(declared.model.content_model())
                .map_err(store_effect)?;
            // The scoped requeue meter records these reads before publication.
            count_increment_work(&request.finish(), 0);
            ops.evidence.count_requeue_links_rejudged(links);
        }
        ops.evidence.count_requeue(
            taken.roots.len() as u64,
            documents,
            store.construction_write_commits() - commits,
            began.elapsed(),
        );
        Ok::<_, JobFailure>(())
    });
    ops.evidence.count_requeue_read_steps(steps);
    result?;
    publisher.publish_requeued(taken);
    #[cfg(feature = "induced-failure")]
    ops.loading_boundary(LoadingBoundary::Requeued);
    Ok(())
}

/// The step scope a drain writes through: the content pass's own pending scope
/// and source, or the structure pass's open group and declaration.
enum Scope<'p, 's> {
    Content(&'p mut Pending<'s>, &'p norn_fs::Vault),
    Structure(&'p mut Store, &'p Declared),
}

impl Scope<'_, '_> {
    fn store(&mut self) -> &mut Store {
        match self {
            Self::Content(pending, _) => pending.store,
            Self::Structure(store, _) => store,
        }
    }
}

/// Pull the Get targets live reads wait on ahead of the queue, inside the
/// open step group, then publish the ones it settled ready.
///
/// Per target, candidate resolution and the content step repeat until the
/// class holds no provisional name — a quarantined exact candidate can expose
/// suffix candidates ([`norn_store::Request::target_candidates`]) — and then
/// the resolved document runs its structure step. Content runs through the
/// pass's own pending scope and walk account, so the job's close sees every
/// withhold and vacate. Every target shares one budget of a host chunk, and
/// never more than the group still admits, so the pass advances a chunk per
/// drain; a class over one chunk is left to the vault-wide step, and a target
/// not settled within the budget stays queued for a later boundary.
///
/// **The group commits before any target publishes**, so the reader's
/// snapshot sees its documents. A drain that wrote and readied nothing commits
/// nothing, so a build with no request pending commits as the pass alone does.
///
/// Content findings alone use the increment path outside groups. Directory
/// sets remain deferred to the Relations judgment.
fn drain(
    ops: &ProductionEntryOps,
    publisher: &Publisher<'_>,
    scope: &mut Scope<'_, '_>,
) -> Result<(), JobFailure> {
    let targets = publisher.pending_targets();
    if targets.is_empty() {
        return Ok(());
    }
    let ignore = match scope {
        Scope::Content(pending, _) => pending.declared.model.content_model(),
        Scope::Structure(_, declared) => declared.content_model(),
    }
    .ambiguity_ignore()
    .clone();
    let chunk = ops.policy.changeset_size;
    let mut budget = chunk.min(scope.store().construction_group_room());
    let mut wrote = 0;
    let mut ready = Vec::new();
    for target in targets {
        if settle(ops, scope, &target, &ignore, chunk, &mut budget, &mut wrote)? {
            ready.push(target);
        }
    }
    if let Scope::Content(pending, _) = scope {
        pending.flush()?;
    }
    if (wrote > 0 || !ready.is_empty())
        && scope.store().commit_step_group().map_err(store_effect)?
    {
        ops.evidence.count_construction_commit_for_requests();
    }
    ops.evidence.count_pull_ahead_documents(wrote);
    publisher.publish_targets(&ready);
    #[cfg(feature = "induced-failure")]
    ops.loading_boundary(LoadingBoundary::RequestsDrained);
    Ok(())
}

/// Run `target`'s owed steps within `budget`, counting what it wrote; whether
/// every candidate is past step 2 and the resolved document past step 3.
fn settle(
    ops: &ProductionEntryOps,
    scope: &mut Scope<'_, '_>,
    target: &str,
    ignore: &AmbiguityIgnore,
    chunk: usize,
    budget: &mut usize,
    wrote: &mut u64,
) -> Result<bool, JobFailure> {
    let candidates = loop {
        let Some(candidates) = scope
            .store()
            .begin_request()
            .target_candidates(target, ignore, chunk)
            .map_err(store_effect)?
        else {
            return Ok(false);
        };
        let owed: Vec<DocumentPath> = candidates
            .iter()
            .filter(|candidate| candidate.owes == Some(Step::Content))
            .map(|candidate| candidate.path.clone())
            .collect();
        if owed.is_empty() {
            break candidates;
        }
        // Content exists only in step 2; a later step owes none.
        let Scope::Content(pending, source) = scope else {
            return Ok(false);
        };
        if owed.len() > *budget {
            return Ok(false);
        }
        content_chunk(ops, pending, source, &owed)?;
        pending.flush()?;
        *budget -= owed.len();
        *wrote += owed.len() as u64;
    };
    let [resolved] = candidates.as_slice() else {
        return Ok(true);
    };
    if resolved.owes != Some(Step::Structure) {
        return Ok(true);
    }
    if *budget == 0 {
        return Ok(false);
    }
    let (store, declared) = match scope {
        Scope::Content(pending, _) => (&mut *pending.store, &pending.declared.model),
        Scope::Structure(store, declared) => (&mut **store, *declared),
    };
    let input = store
        .begin_request()
        .structure_document(&resolved.path)
        .map_err(store_effect)?;
    if let Some(input) = input {
        structure_chunk(store, vec![input], declared)?;
        *budget -= 1;
        *wrote += 1;
    }
    Ok(true)
}

/// Derive an arbitrary set of at most one host chunk, with no held-row input.
/// The vault pass supplies the stage-1 queue a chunk at a time, and NORN-468
/// pull-ahead ([`drain`]) and requeue select arbitrary touched paths here too.
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
/// next. The vault pass supplies a page, and NORN-468 pull-ahead ([`drain`]) a
/// requested target's resolved document; requeue supplies touched documents here too.
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
fn boundary(ops: &ProductionEntryOps, publisher: &Publisher<'_>, step: Step) {
    publisher.publish(step);
    #[cfg(feature = "induced-failure")]
    ops.loading_boundary(LoadingBoundary::StepComplete(step));
    #[cfg(not(feature = "induced-failure"))]
    let _ = ops;
}
