//! Feature-only canonical loading experiment. Ordinary admission stays unchanged.
use super::*;
use norn_fs::watch::FirstIndexObservation;
use norn_store::four_step::{self, PendingDocument, Step, StepCounters};
use norn_store::{DocumentFacts, Snapshot, SnapshotReader};

/// Completed SQL work, plus whether source churn required an ordinary heal.
#[derive(Clone, Debug, Default)]
pub struct FourStepReport {
    pub steps: BTreeMap<&'static str, BTreeMap<&'static str, u64>>,
    pub reconciled: bool,
    /// Descriptive timings from the lab build's start; they grant no read admission.
    pub request_ns: BTreeMap<String, u128>,
    pub refused_ns: BTreeMap<String, u128>,
    pub request_refusals: BTreeMap<String, String>,
    /// Timing bookkeeping is a subset of total commits and elapsed busy time.
    pub timing_commits: BTreeMap<&'static str, u64>,
    pub timing_ns: BTreeMap<&'static str, u128>,
    pub delivery_ns: BTreeMap<String, u128>,
    pub requested_structure_ns: BTreeMap<String, u128>,
    pub content_complete_ns: u128,
    /// Monotonic time since the build began, captured before observer query work.
    pub boundary_ns: BTreeMap<&'static str, u128>,
    /// Main-pass work only; requested-document pull-ahead is recorded separately.
    pub busy_ns: BTreeMap<&'static str, u128>,
    pub pull_ahead_ns: u128,
}

/// All callbacks run after the named transaction committed, outside host gates.
#[derive(Clone, Copy, Debug)]
pub enum FourStepEvent<'a> {
    NamesComplete,
    ContentComplete,
    Structured(&'a str),
    GetReady(&'a str),
    RequestRefused {
        path: &'a str,
        reason: &'a str,
    },
    StructureComplete,
    RelationsComplete,
    FullTextComplete,
    Batch {
        step: &'static str,
        completed: u64,
        total: u64,
    },
    Complete(&'a FourStepReport),
}

pub trait FourStepObserver: Send + Sync {
    fn at(&self, event: FourStepEvent<'_>, read: &FourStepRead<'_>) -> Result<(), String>;
}

/// Lab controls for batch size, body storage, FTS mode, and queued requests.
#[derive(Clone)]
pub struct FourStepLoading {
    bound: usize,
    no_body: bool,
    requests: FourStepRequests,
    fts_mode: four_step::FtsBuildMode,
    observer: Arc<dyn FourStepObserver>,
}

/// At most 256 distinct pending Get requests, drained at each committed batch.
/// Clones share the queue; a full queue refuses rather than silently dropping work.
#[derive(Clone, Default)]
pub struct FourStepRequests {
    pending: Arc<std::sync::Mutex<RequestQueue>>,
}

#[derive(Default)]
struct RequestQueue {
    closed: bool,
    pending: std::collections::VecDeque<(DocumentPath, Instant)>,
    in_flight: std::collections::BTreeSet<DocumentPath>,
}

impl FourStepRequests {
    pub const MAX_PENDING: usize = 256;

    pub fn request(&self, path: String) -> Result<(), String> {
        let path = DocumentPath::new(&path).map_err(|error| error.to_string())?;
        if !is_markdown(Path::new(path.as_str())) {
            return Err("a requested path must name a Markdown document".into());
        }
        let mut pending = self.pending.lock().map_err(|error| error.to_string())?;
        if pending.closed {
            return Err("vault still indexing for the first time: lab intake is closed; retry through the public host after Ready".into());
        }
        if pending.in_flight.contains(&path)
            || pending.pending.iter().any(|(queued, _)| queued == &path)
        {
            return Ok(());
        }
        if pending.pending.len() + pending.in_flight.len() == Self::MAX_PENDING {
            return Err(
                "vault still indexing for the first time: lab request queue is full (256)".into(),
            );
        }
        pending.pending.push_back((path, Instant::now()));
        Ok(())
    }
}

impl FourStepLoading {
    pub fn new(
        bound: usize,
        no_body: bool,
        requested: Vec<String>,
        observer: Arc<dyn FourStepObserver>,
    ) -> Result<Self, String> {
        let maximum = MAX_CHANGESET_SIZE.min(norn_store::MAX_PAGE);
        if bound == 0 || bound > maximum {
            return Err(format!(
                "four-step transaction bound must be in 1..={maximum}"
            ));
        }
        let requests = FourStepRequests::default();
        for path in requested {
            requests.request(path)?;
        }
        Ok(Self {
            bound,
            no_body,
            requests,
            fts_mode: four_step::FtsBuildMode::Rebuild,
            observer,
        })
    }
    pub fn requests(&self) -> FourStepRequests {
        self.requests.clone()
    }

    pub fn with_fts_mode(mut self, mode: four_step::FtsBuildMode) -> Self {
        self.fts_mode = mode;
        self
    }
}

/// Ephemeral lab read hook over the canonical store's ordinary pooled readers.
/// It publishes no lifecycle capability: the callback must finish its read
/// before construction advances. First-index authority is checked after query
/// work as well as before acquisition, including data-derived refusals.
pub struct FourStepRead<'a> {
    reader: &'a Arc<SnapshotReader>,
    model: &'a ContentModel,
    authority: Option<&'a FirstIndexObservation>,
    ready_path: Option<&'a str>,
    metadata_complete: bool,
    search_complete: bool,
    relations_complete: bool,
}

impl FourStepRead<'_> {
    pub fn link_resolution_loading(&self) -> bool {
        !self.relations_complete
    }

    pub fn link_health_loading(&self) -> bool {
        !self.relations_complete
    }

    /// The lab issues no writes; this is the same readiness admission it requires.
    pub fn admit_write(&self) -> Result<(), String> {
        if !self.search_complete {
            return Err(
                "vault still indexing for the first time: writes wait for full-text completion"
                    .into(),
            );
        }
        Ok(())
    }

    pub fn search_complete(&self) -> bool {
        self.search_complete
    }

    pub fn search(&self, query: &norn_store::LexicalQuery) -> Result<norn_store::Searched, String> {
        if !self.search_complete || !norn_store::loading::supports_search(query) {
            return Err("vault still indexing for the first time: full-text search is unavailable at this lab boundary".into());
        }
        let result = self
            .snapshot()?
            .search(query, self.model)
            .map_err(|error| error.to_string());
        if !self.is_current() {
            return Err(
                "vault still indexing for the first time: four-step source changed during search"
                    .into(),
            );
        }
        result
    }

    pub fn metadata_complete(&self) -> bool {
        self.metadata_complete
    }

    pub fn is_current(&self) -> bool {
        self.authority
            .is_some_and(|authority| authority.check_root() && authority.admits())
    }

    fn snapshot(&self) -> Result<Snapshot, String> {
        if !self.is_current() {
            return Err("vault still indexing for the first time: four-step source changed".into());
        }
        self.reader
            .acquire(
                norn_store::ReadNeed::Source,
                Instant::now() + Duration::from_secs(5),
            )
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                "vault still indexing for the first time: four-step reader capacity unavailable"
                    .to_owned()
            })?
            .establish()
            .map_err(|error| error.to_string())
    }

    pub fn get(&self, params: &norn_wire::GetParams) -> Result<norn_store::Gotten, String> {
        let Some(ready_path) = self.ready_path else {
            return Err("vault still indexing for the first time: no document finished structure at this boundary".into());
        };
        let snapshot = self.snapshot()?;
        let result = snapshot
            .get(params, self.model, &crate::text::TextLayer)
            .map_err(|error| error.to_string());
        if !self.is_current() {
            return Err(
                "vault still indexing for the first time: four-step source changed during Get"
                    .into(),
            );
        }
        let result = result?;
        let path = match &result.report {
            norn_wire::GetReport::Record { document, .. } => document.path.as_str(),
            norn_wire::GetReport::Section { path, .. }
            | norn_wire::GetReport::Block { path, .. }
            | norn_wire::GetReport::Collection { path, .. } => path.as_str(),
            _ => return Err("unsupported lab Get report".into()),
        };
        if path != ready_path {
            return Err("vault still indexing for the first time: the requested document has not completed this structure callback".into());
        }
        Ok(result)
    }

    pub fn find(&self, params: &norn_wire::FindParams) -> Result<norn_store::Found, String> {
        if !self.metadata_complete || !norn_store::loading::supports_find(params) {
            return Err("vault still indexing for the first time: only complete field/path Find is available at this lab boundary".into());
        }
        let result = self
            .snapshot()?
            .find(params, self.model)
            .map_err(|error| error.to_string());
        if !self.is_current() {
            return Err(
                "vault still indexing for the first time: four-step source changed during Find"
                    .into(),
            );
        }
        result
    }
}

impl ProductionEntryOps {
    pub fn with_four_step_loading(mut self, loading: FourStepLoading) -> Self {
        self.four_step = Some(loading);
        self
    }

    pub(super) fn four_step_heal(
        &self,
        attachment: &mut ProductionAttachment,
        progress: &Healing<'_, ProductionAttachment>,
        loading: &FourStepLoading,
    ) -> Result<(), JobFailure> {
        let root = &attachment.covered_root;
        let exclusions = exclusions_at(&attachment.registration, &attachment.shadows, root);
        let source = norn_fs::Vault::open(root, &exclusions).map_err(effect)?;
        let authority = attachment
            .subscription
            .as_ref()
            .and_then(Subscription::first_index_observation);
        let reader = Arc::new(attachment.store.reader_pool(&self.readers));
        let declared = &attachment.pinned;
        let mut build = Build {
            ops: self,
            store: &mut attachment.store,
            root,
            exclusions: &exclusions,
            source: &source,
            declared,
            loading,
            authority: authority.as_ref(),
            reader: &reader,
            report: FourStepReport::default(),
            changed: false,
            metadata_complete: false,
            search_complete: false,
            relations_complete: false,
            skipped_links: SkippedLinks::default(),
            began: Instant::now(),
            observer_ns: std::cell::Cell::new(0),
            total_documents: 0,
            structured_documents: 0,
        };
        let counters = four_step::begin(build.store).map_err(store_effect)?;

        for step in ["names", "content", "structure", "relations", "full-text"] {
            build.add(step, StepCounters::default());
        }
        build.add("names", counters);
        let names_began = Instant::now();
        let names_observer = build.observer_ns.get();
        build.names()?;
        build.report.busy_ns.insert(
            "names",
            names_began.elapsed().as_nanos() - (build.observer_ns.get() - names_observer),
        );
        build.boundary("names");
        build.observe(FourStepEvent::NamesComplete, None)?;
        build.drain_requests()?;
        let content_began = Instant::now();
        let content_observer = build.observer_ns.get();
        let content_pull_ahead = build.report.pull_ahead_ns;
        loop {
            let documents = four_step::pending(build.store, Step::Content, loading.bound)
                .map_err(store_effect)?;
            if documents.is_empty() {
                break;
            }
            build.content(documents)?;
            build.batch("content")?;
            build.drain_requests()?;
        }
        build.report.busy_ns.insert(
            "content",
            content_began.elapsed().as_nanos()
                - (build.observer_ns.get() - content_observer)
                - (build.report.pull_ahead_ns - content_pull_ahead),
        );
        build.metadata_complete = true;
        build.boundary("content");
        build.report.content_complete_ns = build.began.elapsed().as_nanos();
        build.observe(FourStepEvent::ContentComplete, None)?;
        build.drain_requests()?;
        let structure_began = Instant::now();
        let structure_observer = build.observer_ns.get();
        let structure_pull_ahead = build.report.pull_ahead_ns;
        loop {
            let documents = four_step::pending(build.store, Step::Structure, loading.bound)
                .map_err(store_effect)?;
            if documents.is_empty() {
                // A fully pulled-ahead or empty vault can still owe refused spellings.
                if four_step::has_refused(build.store).map_err(store_effect)? {
                    build.structure(Vec::new())?;
                    build.batch("structure")?;
                    build.drain_requests()?;
                    continue;
                }
                break;
            }
            build.structure(documents)?;
            build.batch("structure")?;
            build.drain_requests()?;
        }
        build.report.busy_ns.insert(
            "structure",
            structure_began.elapsed().as_nanos()
                - (build.observer_ns.get() - structure_observer)
                - (build.report.pull_ahead_ns - structure_pull_ahead),
        );
        build.boundary("structure");
        build.observe(FourStepEvent::StructureComplete, None)?;
        build.drain_requests()?;
        let relations_began = Instant::now();
        let relations_observer = build.observer_ns.get();
        let relations_pull_ahead = build.report.pull_ahead_ns;
        loop {
            let counters = four_step::health(build.store, loading.bound, declared.content_model())
                .map_err(store_effect)?;
            if counters.commits == 0 {
                break;
            }
            build.add("relations", counters);
            build.batch("relations")?;
            build.drain_requests()?;
        }
        attachment.skipped_links = std::mem::take(&mut build.skipped_links);
        // Every surviving row has complete content/structure before ordinary
        // hash-based reconciliation can skip it. A changed no-body source was
        // forgotten instead of pairing new body bytes with old frontmatter.
        if build.changed
            || authority
                .as_ref()
                .is_none_or(|proof| !proof.check_root() || !proof.admits())
        {
            build.report.reconciled = true;
            attachment.skipped_links =
                heal_documents(build.store, root, &exclusions, self.policy, progress)?;
        }
        build.report.busy_ns.insert(
            "relations",
            relations_began.elapsed().as_nanos()
                - (build.observer_ns.get() - relations_observer)
                - (build.report.pull_ahead_ns - relations_pull_ahead),
        );
        build.relations_complete = true;
        build.boundary("relations");
        build.observe(FourStepEvent::RelationsComplete, None)?;
        build.drain_requests()?;
        let fts_began = Instant::now();
        let fts_observer = build.observer_ns.get();
        let fts_pull_ahead = build.report.pull_ahead_ns;
        loop {
            let counters = four_step::full_text(build.store, loading.fts_mode, loading.bound)
                .map_err(store_effect)?;
            if counters.commits == 0 {
                break;
            }
            build.add("full-text", counters);
            build.batch("full-text")?;
            build.drain_requests()?;
        }
        build.drain_and_close_requests()?;
        let counters = four_step::finish(build.store).map_err(store_effect)?;
        build.add("full-text", counters);
        build.report.busy_ns.insert(
            "full-text",
            fts_began.elapsed().as_nanos()
                - (build.observer_ns.get() - fts_observer)
                - (build.report.pull_ahead_ns - fts_pull_ahead),
        );
        build.search_complete = true;
        build.boundary("full-text");
        build.observe(FourStepEvent::FullTextComplete, None)?;
        build.observe(FourStepEvent::Complete(&build.report), None)?;
        Ok(())
    }
}

struct Build<'a> {
    ops: &'a ProductionEntryOps,
    store: &'a mut Store,
    root: &'a Path,
    exclusions: &'a [PathBuf],
    source: &'a norn_fs::Vault,
    declared: &'a Declared,
    loading: &'a FourStepLoading,
    authority: Option<&'a FirstIndexObservation>,
    reader: &'a Arc<SnapshotReader>,
    report: FourStepReport,
    changed: bool,
    metadata_complete: bool,
    search_complete: bool,
    relations_complete: bool,
    skipped_links: SkippedLinks,
    began: Instant,
    observer_ns: std::cell::Cell<u128>,
    total_documents: u64,
    structured_documents: u64,
}

impl Build<'_> {
    fn batch(&self, step: &'static str) -> Result<(), JobFailure> {
        let completed = match step {
            "content" => self.report.steps["content"]["files_read"],
            "structure" => self.structured_documents,
            "full-text" => self.report.steps["full-text"]["fts_rows_inserted"],
            _ => self.report.steps[step]["commits"],
        };
        self.observe(
            FourStepEvent::Batch {
                step,
                completed,
                total: self.total_documents,
            },
            None,
        )
    }

    // Close only while the queue is empty under the intake lock. Requests
    // accepted by final-batch delivery callbacks therefore still get a turn.
    fn drain_and_close_requests(&mut self) -> Result<(), JobFailure> {
        loop {
            {
                let mut pending = self
                    .loading
                    .requests
                    .pending
                    .lock()
                    .map_err(|error| JobFailure::Environmental(error.to_string()))?;
                if pending.pending.is_empty() && pending.in_flight.is_empty() {
                    pending.closed = true;
                    return Ok(());
                }
            }
            self.drain_requests()?;
        }
    }

    fn drain_requests(&mut self) -> Result<(), JobFailure> {
        let requests = {
            let mut pending = self
                .loading
                .requests
                .pending
                .lock()
                .map_err(|error| JobFailure::Environmental(error.to_string()))?;
            let requests = pending.pending.drain(..).collect::<Vec<_>>();
            pending
                .in_flight
                .extend(requests.iter().map(|(path, _)| path.clone()));
            requests
        };
        let began = Instant::now();
        let observer = self.observer_ns.get();
        for (path, requested_at) in requests {
            let name = path.as_str().to_owned();
            self.report.request_ns.insert(
                name.clone(),
                requested_at
                    .checked_duration_since(self.began)
                    .map_or(0, |d| d.as_nanos()),
            );
            if let Some(document) =
                four_step::pending_at(self.store, Step::Content, &path).map_err(store_effect)?
            {
                self.content(vec![document])?;
            }
            if let Some(document) =
                four_step::pending_at(self.store, Step::Structure, &path).map_err(store_effect)?
            {
                self.structure(vec![document])?;
            } else if four_step::structure_ns(self.store, &path)
                .map_err(store_effect)?
                .is_some()
            {
                self.observe(FourStepEvent::GetReady(path.as_str()), Some(path.as_str()))?;
            }
            if let Some(structured_at) =
                four_step::structure_ns(self.store, &path).map_err(store_effect)?
            {
                self.report
                    .requested_structure_ns
                    .insert(name.clone(), structured_at);
                self.report
                    .delivery_ns
                    .insert(name, self.began.elapsed().as_nanos());
            } else {
                let reason = "vault still indexing for the first time: requested path has no current completed construction row";
                self.observe(
                    FourStepEvent::RequestRefused {
                        path: path.as_str(),
                        reason,
                    },
                    None,
                )?;
                self.report
                    .request_refusals
                    .insert(name.clone(), reason.to_owned());
                self.report
                    .refused_ns
                    .insert(name, self.began.elapsed().as_nanos());
            }
            self.loading
                .requests
                .pending
                .lock()
                .map_err(|error| JobFailure::Environmental(error.to_string()))?
                .in_flight
                .remove(&path);
        }
        self.report.pull_ahead_ns +=
            began.elapsed().as_nanos() - (self.observer_ns.get() - observer);
        Ok(())
    }

    fn boundary(&mut self, step: &'static str) {
        self.report
            .boundary_ns
            .insert(step, self.began.elapsed().as_nanos());
    }

    fn add(&mut self, step: &'static str, counters: StepCounters) {
        let values = readings(&counters);
        let total = self.report.steps.entry(step).or_default();
        for (name, count) in values {
            *total.entry(name).or_default() += count;
        }
        self.ops.evidence.count_four_step(step, &counters);
    }

    fn observe(
        &self,
        event: FourStepEvent<'_>,
        ready_path: Option<&str>,
    ) -> Result<(), JobFailure> {
        let began = Instant::now();
        let result = self
            .loading
            .observer
            .at(
                event,
                &FourStepRead {
                    reader: self.reader,
                    model: self.declared.content_model(),
                    authority: self.authority,
                    ready_path,
                    metadata_complete: self.metadata_complete,
                    search_complete: self.search_complete,
                    relations_complete: self.relations_complete,
                },
            )
            .map_err(JobFailure::Environmental);
        self.observer_ns
            .set(self.observer_ns.get() + began.elapsed().as_nanos());
        result
    }

    fn changed(&mut self) {
        self.changed = true;
        if let Some(proof) = self.authority {
            proof.invalidate();
        }
    }

    fn names(&mut self) -> Result<(), JobFailure> {
        let mut documents = Vec::new();
        let mut files = Vec::new();
        let mut refused = Vec::new();
        for fact in walk(self.root, self.exclusions).map_err(effect)? {
            match fact.map_err(effect)? {
                norn_fs::WalkFact::File(file) => match document_path(file.path().as_path()) {
                    Ok(path) => {
                        if is_markdown(file.path().as_path()) {
                            documents.push(path);
                        } else if path.is_vault_file() {
                            files.push(path);
                        }
                    }
                    Err(quarantine) if is_markdown(file.path().as_path()) => {
                        let finding =
                            Queued::new(plan_quarantine(file.path().as_path(), quarantine), None);
                        refused.push(finding.finding);
                    }
                    Err(_) => {}
                },
                norn_fs::WalkFact::Skipped(skip) => {
                    if matches!(skip.reason(), norn_fs::SkipReason::SymbolicLink(_)) {
                        self.skipped_links.saw(skip.path().as_path());
                    }
                    if !skip.reason().stands() {
                        self.changed();
                    }
                }
            }
            if documents.len() + files.len() + refused.len() >= self.loading.bound {
                let counters = four_step::names_with_refused(
                    self.store,
                    &documents,
                    &files,
                    &refused,
                    self.declared.content_model(),
                )
                .map_err(store_effect)?;

                self.total_documents += documents.len() as u64;
                self.add("names", counters);
                documents.clear();
                files.clear();
                refused.clear();
            }
        }
        if !documents.is_empty() || !files.is_empty() || !refused.is_empty() {
            let counters = four_step::names_with_refused(
                self.store,
                &documents,
                &files,
                &refused,
                self.declared.content_model(),
            )
            .map_err(store_effect)?;

            self.total_documents += documents.len() as u64;
            self.add("names", counters);
        }
        Ok(())
    }

    fn read(&mut self, path: &DocumentPath) -> Result<Option<norn_fs::ReadFile>, JobFailure> {
        match self.source.fact(Path::new(path.as_str())).map_err(effect)? {
            norn_fs::WalkFact::File(file) => file.read_optional().map_err(effect),
            norn_fs::WalkFact::Skipped(_) => Ok(None),
        }
    }

    fn content(&mut self, documents: Vec<PendingDocument>) -> Result<(), JobFailure> {
        let mut facts = Vec::new();
        let mut offsets = Vec::new();
        let mut findings = Vec::new();
        let mut files_read = 0;
        for document in documents {
            files_read += 1;
            let Some(read) = self.read(&document.path)? else {
                self.changed();
                let counters =
                    four_step::forget(self.store, &document.path).map_err(store_effect)?;

                self.add("content", counters);
                continue;
            };
            count_document_derived();
            let hash = read.content_hash().to_string();
            let (plan, origin) = crate::derivation::plan_four_step_content(
                &document.path,
                read.bytes(),
                hash.clone(),
                self.declared,
                self.store.path_order().glob_case(),
            );
            count_rule_work(plan.rule_work);
            if let Some(Change::Upsert(document)) = plan.change {
                let origin = origin.expect("decoded content carries its origin");
                offsets.push((origin.line as u64 - 1, origin.column as u64 - 1));
                facts.push(document);
            }
            findings.extend(derived_findings(plan.findings, Some(&hash)));
        }
        if !facts.is_empty() || !findings.is_empty() {
            let counters = four_step::content(
                self.store,
                &facts,
                &findings,
                self.declared.content_model(),
                self.loading.no_body,
                &offsets,
            )
            .map_err(store_effect)?;
            self.add("content", counters);
        }
        self.add(
            "content",
            StepCounters {
                files_read,
                ..StepCounters::default()
            },
        );
        Ok(())
    }

    fn structure(&mut self, documents: Vec<PendingDocument>) -> Result<(), JobFailure> {
        let mut facts = Vec::new();
        let mut findings = Vec::new();
        let mut files_read = 0;
        let mut pending = Vec::new();
        for document in documents {
            let body = if self.loading.no_body {
                files_read += 1;
                let read = self.read(&document.path)?;
                if read
                    .as_ref()
                    .is_none_or(|read| read.content_hash().to_string() != document.content_hash)
                {
                    self.changed();
                    let counters =
                        four_step::forget(self.store, &document.path).map_err(store_effect)?;

                    self.add("structure", counters);
                    continue;
                }
                let read = read.expect("verified source");
                let text = crate::derivation::document_source(read.bytes())
                    .expect("hash-verified decodable bytes");
                text[document.body_offset as usize..].to_owned()
            } else {
                document.body.clone()
            };
            let mut fact = DocumentFacts::new(
                document.path.clone(),
                document.content_hash.clone(),
                body,
                document.byte_length,
            );
            fact.body_offset = document.body_offset;
            crate::derivation::four_step_structure(
                &mut fact,
                norn_text::SourceSpan {
                    byte_offset: document.body_offset as usize,
                    line: document.body_line_offset as usize + 1,
                    column: document.body_column_offset as usize + 1,
                },
            );
            if self.declared.schema().tags().reports_undeclared() {
                let mut tags = document
                    .frontmatter_tag_names
                    .iter()
                    .map(|name| norn_store::TagFact {
                        name: name.clone(),
                        source: norn_store::TagSource::Frontmatter,
                        span: None,
                    })
                    .collect::<Vec<_>>();
                tags.append(&mut fact.tags);
                fact.tags = tags;
                findings.extend(derived_findings(
                    crate::derivation::four_step_tag_findings(&fact, self.declared),
                    None,
                ));
                fact.tags
                    .retain(|tag| tag.source == norn_store::TagSource::Body);
            }
            facts.push(fact);
            pending.push(document);
        }
        // Deferred names are held in SQLite and consumed in bounded pages,
        // inside the same structure transaction as the body and tag findings.
        if !facts.is_empty()
            || (self.metadata_complete
                && four_step::has_refused(self.store).map_err(store_effect)?)
        {
            let counters = four_step::structure_with_refused(
                self.store,
                &facts,
                &pending,
                &findings,
                self.declared.content_model(),
                self.loading.no_body,
                if self.metadata_complete {
                    self.loading.bound
                } else {
                    0
                },
            )
            .map_err(store_effect)?;
            self.add("structure", counters);
        }
        self.add(
            "structure",
            StepCounters {
                files_read,
                ..StepCounters::default()
            },
        );
        let structured_at = self.began.elapsed().as_nanos();
        let timing_began = Instant::now();
        four_step::record_structure_ns(
            self.store,
            &facts.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
            structured_at,
        )
        .map_err(store_effect)?;
        self.structured_documents += facts.len() as u64;
        if !facts.is_empty() {
            *self.report.timing_ns.entry("structure").or_default() +=
                timing_began.elapsed().as_nanos();
            *self.report.timing_commits.entry("structure").or_default() += 1;
            self.add(
                "structure",
                StepCounters {
                    commits: 1,
                    ..StepCounters::default()
                },
            );
        }
        for fact in &facts {
            self.observe(
                FourStepEvent::Structured(fact.path.as_str()),
                Some(fact.path.as_str()),
            )?;
            self.observe(
                FourStepEvent::GetReady(fact.path.as_str()),
                Some(fact.path.as_str()),
            )?;
        }
        Ok(())
    }
}

fn derived_findings(
    planned: Vec<PlannedFinding>,
    hash: Option<&str>,
) -> Vec<DerivedFinding<'static>> {
    planned
        .into_iter()
        .map(|finding| {
            let bytes_hash = (finding.cause.kind() == norn_store::CONTENT_FINDING_KIND)
                .then(|| hash.expect("quarantine carries its bytes hash").to_owned());
            let queued = Queued::new(finding, bytes_hash);
            DerivedFinding {
                facts: queued.finding,
                replaces: None,
                bytes_hash: queued.bytes_hash,
            }
        })
        .collect()
}

pub(crate) fn readings(counters: &StepCounters) -> BTreeMap<&'static str, u64> {
    BTreeMap::from([
        ("rows_inserted", counters.rows_inserted),
        ("rows_rewritten", counters.rows_rewritten),
        ("body_rows_rewritten", counters.body_rows_rewritten),
        ("rows_deleted", counters.rows_deleted),
        ("tombstones_discarded", counters.tombstones_discarded),
        ("fact_rows_written", counters.fact_rows_written),
        ("links_judged", counters.links_judged),
        ("link_health_keys_resolved", counters.keys_resolved),
        ("link_health_candidates_read", counters.candidates_read),
        ("findings_written", counters.findings_written),
        ("findings_discarded", counters.findings_discarded),
        ("commits", counters.commits),
        ("files_read", counters.files_read),
        ("fts_rows_inserted", counters.fts_rows_inserted),
    ])
}

impl crate::Host<ProductionEntryOps> {
    /// Committed phase counters from this host's lab builds.
    pub fn four_step_steps(&self) -> BTreeMap<&'static str, BTreeMap<&'static str, u64>> {
        self.ops().evidence.four_step_steps()
    }
}
