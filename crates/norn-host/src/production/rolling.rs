//! A rolling heal advances one bounded piece per maintenance claim.

use super::*;
use norn_fs::walk::inventory::InventoryWalk;
use std::time::SystemTime;

pub(super) const PASS_PERIOD: Duration = Duration::from_secs(3600);
pub(super) const DELIVERY_GRACE: Duration = Duration::from_secs(5);
const DOCUMENT_BUDGET: usize = 64;
const BYTE_BUDGET: u64 = 1024 * 1024;

pub(super) struct RollingHeal {
    pub(super) vault: norn_fs::Vault,
    pub due: Instant,
    period: Duration,
    pub last_completed: Option<SystemTime>,
    pub misses: norn_wire::WatcherMisses,
    phase: Phase,
    inventory: Option<InventoryWalk>,
    cursor: Vec<u8>,
    checked: u64,
    enumerated: u64,
    findings_after: Option<DocumentPath>,
}

#[derive(Clone, Copy)]
enum Phase {
    Idle,
    Preparing,
    Healing,
    Revisit,
    Findings,
    Cleanup,
    Accounts,
}

impl RollingHeal {
    pub fn new(vault: norn_fs::Vault, period: Duration) -> Self {
        Self {
            vault,
            due: Instant::now() + period,
            period,
            last_completed: None,
            misses: norn_wire::WatcherMisses::default(),
            phase: Phase::Idle,
            inventory: None,
            cursor: Vec::new(),
            checked: 0,
            enumerated: 0,
            findings_after: None,
        }
    }

    pub fn due(&self) -> bool {
        !matches!(self.phase, Phase::Idle) || Instant::now() >= self.due
    }
    pub fn status(&self) -> norn_wire::RollingHealStatus {
        let seconds = |time: SystemTime| {
            time.duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        };
        let phase = match self.phase {
            Phase::Idle => None,
            Phase::Preparing => Some(norn_wire::RollingHealPhase::Preparing),
            Phase::Healing => Some(norn_wire::RollingHealPhase::Healing),
            _ => Some(norn_wire::RollingHealPhase::Finishing),
        };
        norn_wire::RollingHealStatus {
            pass: phase.map(|phase| norn_wire::RollingHealProgress {
                phase,
                entries_enumerated: self.enumerated,
                entries_checked: self.checked,
            }),
            last_completed_unix_seconds: self.last_completed.map(seconds),
            next_due_unix_seconds: seconds(
                SystemTime::now() + self.due.saturating_duration_since(Instant::now()),
            ),
            watcher_misses: self.misses,
        }
    }

    pub fn reset_after_rebuild(&mut self) {
        self.inventory = None;
        self.phase = Phase::Idle;
        self.cursor.clear();
        self.findings_after = None;
        while self.due <= Instant::now() {
            self.due += self.period;
        }
    }
}

pub(super) fn slice(
    attachment: &mut ProductionAttachment,
    policy: ProductionPolicy,
) -> Result<(), JobFailure> {
    if !attachment.rolling.due() {
        return Ok(());
    }
    let exclusions = shadow_exclusions_for(attachment);
    let root = &attachment.covered_root;
    let rolling = &mut attachment.rolling;
    let store = &mut attachment.store;
    if matches!(rolling.phase, Phase::Idle) {
        store
            .begin_request()
            .prepare_inventory()
            .map_err(store_effect)?;
        rolling.inventory = Some(InventoryWalk::from_vault(&rolling.vault).map_err(effect)?);
        rolling.cursor.clear();
        rolling.checked = 0;
        rolling.enumerated = 0;
        rolling.phase = Phase::Preparing;
    }
    match rolling.phase {
        Phase::Preparing => {
            let page = rolling
                .inventory
                .as_mut()
                .expect("an active walk")
                .advance(DOCUMENT_BUDGET)
                .map_err(effect)?;
            rolling.enumerated += page.visited;
            for record in page.records {
                store
                    .begin_request()
                    .inventory_record(&record.key, &record.value)
                    .map_err(store_effect)?;
            }
            if page.complete {
                rolling.phase = Phase::Healing;
            }
        }
        Phase::Healing => {
            let page = store
                .begin_request()
                .inventory_after(Some(&rolling.cursor), DOCUMENT_BUDGET)
                .map_err(store_effect)?;
            let upper = page.last().map(|(key, _)| key.clone());
            let inventory = rolling.inventory.as_ref().expect("an active walk");
            let facts = page
                .into_iter()
                .map(|(key, record)| inventory.replay(&record).map(|fact| (key, fact)));
            let scope = HealScope::Window {
                after: &rolling.cursor,
                through: upper.as_deref(),
            };
            let mut account = Account {
                rolling: true,
                ..Account::default()
            };
            let result = merge_window(
                store,
                root,
                &exclusions,
                facts,
                inventory.case_sensitivity(),
                scope,
                policy,
                None,
                &mut account,
                Some(&rolling.vault),
                Some(MergeLimit {
                    documents: DOCUMENT_BUDGET,
                    bytes: BYTE_BUDGET,
                }),
            );
            rolling.misses.changed += account.misses.changed;
            rolling.misses.created += account.misses.created;
            rolling.misses.deleted += account.misses.deleted;
            let end = result?;
            rolling.checked += end.checked;
            if !end.through.is_empty() {
                rolling.cursor = end.through;
            }
            if upper.is_none() && end.complete {
                rolling.phase = Phase::Revisit;
                rolling.cursor.clear();
            }
        }
        Phase::Revisit => {
            let page = store
                .begin_request()
                .inventory_after(Some(&rolling.cursor), DOCUMENT_BUDGET)
                .map_err(store_effect)?;
            let mut account = Account {
                rolling: true,
                ..Account::default()
            };
            let mut pending = Pending::new(
                store,
                policy.changeset_size,
                root,
                &exclusions,
                &mut account,
            )?;
            let inventory = rolling.inventory.as_ref().expect("an active walk");
            for (_, record) in &page {
                match inventory.replay(record).map_err(effect)? {
                    norn_fs::WalkFact::File(file) if is_markdown(file.path().as_path()) => {
                        if let Err(quarantine) = document_path(file.path().as_path()) {
                            pending.quarantine(file.path().as_path(), quarantine);
                        }
                    }
                    norn_fs::WalkFact::Skipped(skip) => {
                        pending.passed_over(skip.path().as_path(), skip.reason())
                    }
                    norn_fs::WalkFact::File(_) => {}
                }
                if pending.is_full() {
                    pending.flush()?;
                }
            }
            pending.flush()?;
            persist_account(
                pending.store,
                pending.account,
                stored_path_order(inventory.case_sensitivity()),
            )?;
            if let Some((last, _)) = page.last() {
                rolling.cursor = last.clone();
            } else {
                rolling.phase = Phase::Findings;
                rolling.findings_after = None;
            }
        }
        Phase::Findings => {
            let page = store
                .begin_request()
                .finding_subjects_after(rolling.findings_after.as_ref(), DOCUMENT_BUDGET)
                .map_err(store_effect)?;
            let order = store.path_order();
            for subject in &page {
                if store
                    .begin_request()
                    .stored_document(subject)
                    .map_err(store_effect)?
                    .is_some()
                    || held(store, subject, order)?
                {
                    continue;
                }
                let flags = store
                    .begin_request()
                    .inventory_flags(subject.as_str())
                    .map_err(store_effect)?;
                let known_content = flags & FILED_BYTES == 0
                    && store
                        .begin_request()
                        .content_quarantine_hash(subject)
                        .map_err(store_effect)?
                        .is_some();
                if known_content
                    && matches!(
                        rolling
                            .vault
                            .fact(Path::new(subject.as_str()))
                            .map_err(effect)?,
                        norn_fs::WalkFact::File(file) if file.path().as_path() == Path::new(subject.as_str())
                    )
                {
                    // A file can arrive after enumeration without a row. An
                    // older inventory cannot withdraw its content finding.
                    continue;
                }
                for (side, flag) in [
                    (Decided::ByBytes, FILED_BYTES),
                    (Decided::BySpelling, FILED_SPELLING),
                ] {
                    if flags & flag == 0 {
                        store
                            .begin_request()
                            .discard_findings_about(subject, side.rederives())
                            .map_err(store_effect)?;
                        if side == Decided::ByBytes && known_content {
                            rolling.misses.deleted += 1;
                        }
                    }
                }
            }
            if let Some(last) = page.last() {
                rolling.findings_after = Some(last.clone());
            } else {
                rolling.phase = Phase::Cleanup;
            }
        }
        Phase::Cleanup => {
            let page = store
                .begin_request()
                .inventory_after(None, DOCUMENT_BUDGET)
                .map_err(store_effect)?;
            if let Some((last, _)) = page.last() {
                store
                    .begin_request()
                    .inventory_discard_through(last)
                    .map_err(store_effect)?;
            } else {
                rolling.phase = Phase::Accounts;
            }
        }
        Phase::Accounts => {
            if store
                .begin_request()
                .inventory_clear_accounts(DOCUMENT_BUDGET)
                .map_err(store_effect)?
                == 0
            {
                rolling.inventory = None;
                rolling.phase = Phase::Idle;
                rolling.last_completed = Some(SystemTime::now());
                let now = Instant::now();
                while rolling.due <= now {
                    rolling.due += rolling.period;
                }
            }
        }
        Phase::Idle => {}
    }
    Ok(())
}

fn shadow_exclusions_for(attachment: &ProductionAttachment) -> Vec<PathBuf> {
    exclusions_at(
        &attachment.registration,
        &attachment.shadows,
        &attachment.covered_root,
    )
}

const FILED_BYTES: u32 = 1;
const FILED_SPELLING: u32 = 2;
const WITHHELD: u32 = 4;
const RENDERED: u32 = 32;

pub(super) fn replaced_flag(side: Decided) -> u32 {
    match side {
        Decided::ByBytes => 8,
        Decided::BySpelling => 16,
    }
}

pub(super) fn persist_account(
    store: &mut Store,
    account: &Account,
    order: StoredPathOrder,
) -> Result<(), JobFailure> {
    for (path, sides) in &account.filed.places {
        let flags = sides.iter().fold(0, |flags, side| {
            flags
                | match side {
                    Decided::ByBytes => FILED_BYTES,
                    Decided::BySpelling => FILED_SPELLING,
                }
        });
        store
            .begin_request()
            .inventory_account(path.as_str(), flags)
            .map_err(store_effect)?;
    }
    for root in &account.withheld.roots {
        let root = match order {
            StoredPathOrder::Sensitive => root.clone(),
            StoredPathOrder::AsciiCaseInsensitive => root.to_ascii_lowercase(),
        };
        store
            .begin_request()
            .inventory_account(&root, WITHHELD)
            .map_err(store_effect)?;
    }
    if account.withheld.rendered {
        store
            .begin_request()
            .inventory_account("", RENDERED)
            .map_err(store_effect)?;
    }
    Ok(())
}

fn held(
    store: &mut Store,
    subject: &DocumentPath,
    order: StoredPathOrder,
) -> Result<bool, JobFailure> {
    let place = match order {
        StoredPathOrder::Sensitive => subject.as_str().to_owned(),
        StoredPathOrder::AsciiCaseInsensitive => subject.as_str().to_ascii_lowercase(),
    };
    let global = store
        .begin_request()
        .inventory_flags("")
        .map_err(store_effect)?;
    if global & WITHHELD != 0 || global & RENDERED != 0 && subject.carries_marker() {
        return Ok(true);
    }
    for ancestor in place
        .match_indices('/')
        .map(|(at, _)| &place[..at])
        .chain(std::iter::once(place.as_str()))
    {
        if store
            .begin_request()
            .inventory_flags(ancestor)
            .map_err(store_effect)?
            & WITHHELD
            != 0
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A watcher turn accounts for its own roots. The pass cannot use an older
/// enumeration to withdraw findings that turn has since filed there.
pub(super) fn watcher_turn(
    attachment: &mut ProductionAttachment,
    batch: &norn_fs::Batch,
) -> Result<(), JobFailure> {
    if matches!(attachment.rolling.phase, Phase::Idle) {
        return Ok(());
    }
    if !batch.rescans().is_empty() {
        attachment
            .store
            .begin_request()
            .inventory_account("", WITHHELD)
            .map_err(store_effect)?;
    }
    let mut account = Account::default();
    for root in batch.vault_roots() {
        account.withheld.absorb(root.as_path());
    }
    let order = attachment.store.path_order();
    persist_account(&mut attachment.store, &account, order)
}

pub(super) fn controls_changed(attachment: &mut ProductionAttachment) -> Result<(), JobFailure> {
    if !matches!(attachment.rolling.phase, Phase::Idle) {
        attachment
            .store
            .begin_request()
            .inventory_account("", WITHHELD)
            .map_err(store_effect)?;
    }
    Ok(())
}
