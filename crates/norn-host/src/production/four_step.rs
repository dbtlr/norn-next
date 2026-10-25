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
    pub requested_structure_ns: BTreeMap<String, u128>,
    pub content_complete_ns: u128,
}

/// All callbacks run after the named transaction committed, outside host gates.
#[derive(Clone, Copy, Debug)]
pub enum FourStepEvent<'a> {
    NamesComplete,
    ContentComplete,
    Structured(&'a str),
    GetReady(&'a str),
    StructureComplete,
    Complete(&'a FourStepReport),
}

pub trait FourStepObserver: Send + Sync {
    fn at(&self, event: FourStepEvent<'_>, read: &FourStepRead<'_>) -> Result<(), String>;
}

/// The only lab controls: body variant, transaction bound, and explicit pull-ahead paths.
#[derive(Clone)]
pub struct FourStepLoading {
    bound: usize,
    no_body: bool,
    requested: Vec<DocumentPath>,
    observer: Arc<dyn FourStepObserver>,
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
        let mut paths = Vec::new();
        for path in requested {
            let path = DocumentPath::new(&path).map_err(|error| error.to_string())?;
            if !is_markdown(Path::new(path.as_str())) {
                return Err("a requested path must name a Markdown document".into());
            }
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        Ok(Self {
            bound,
            no_body,
            requested: paths,
            observer,
        })
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
}

impl FourStepRead<'_> {
    pub fn metadata_complete(&self) -> bool {
        self.metadata_complete
    }

    pub fn is_current(&self) -> bool {
        self.authority
            .is_some_and(|authority| authority.check_root() && authority.admits())
    }

    fn snapshot(&self) -> Result<Snapshot, String> {
        if !self.is_current() {
            return Err("four-step source changed".into());
        }
        self.reader
            .acquire(
                norn_store::ReadNeed::Source,
                Instant::now() + Duration::from_secs(5),
            )
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "four-step reader capacity unavailable".to_owned())?
            .establish()
            .map_err(|error| error.to_string())
    }

    pub fn get(&self, params: &norn_wire::GetParams) -> Result<norn_store::Gotten, String> {
        if !self.metadata_complete {
            return Err("whole Get waits for vault-wide content eligibility".into());
        }
        let Some(ready_path) = self.ready_path else {
            return Err("no document finished structure at this boundary".into());
        };
        let snapshot = self.snapshot()?;
        let result = snapshot
            .get(params, self.model, &crate::text::TextLayer)
            .map_err(|error| error.to_string());
        if !self.is_current() {
            return Err("four-step source changed during Get".into());
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
            return Err("the requested document has not completed this structure callback".into());
        }
        Ok(result)
    }

    pub fn find(&self, params: &norn_wire::FindParams) -> Result<norn_store::Found, String> {
        if !self.metadata_complete || !norn_store::loading::supports_find(params) {
            return Err("only complete field/path Find is available at this lab boundary".into());
        }
        let result = self
            .snapshot()?
            .find(params, self.model)
            .map_err(|error| error.to_string());
        if !self.is_current() {
            return Err("four-step source changed during Find".into());
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
            began: Instant::now(),
        };
        let counters = four_step::begin(build.store).map_err(store_effect)?;

        for step in ["names", "content", "structure", "relations"] {
            build.add(step, StepCounters::default());
        }
        build.add("names", counters);
        build.names()?;
        build.observe(FourStepEvent::NamesComplete, None)?;
        // Requested documents use exactly the same content and structure
        // routines as the main passes. Persisted per-path state skips them later.
        for path in &loading.requested {
            if let Some(document) =
                four_step::pending_at(build.store, Step::Content, path).map_err(store_effect)?
            {
                build.content(vec![document])?;
            }
            if let Some(document) =
                four_step::pending_at(build.store, Step::Structure, path).map_err(store_effect)?
            {
                build.structure(vec![document])?;
            }
        }
        loop {
            let documents = four_step::pending(build.store, Step::Content, loading.bound)
                .map_err(store_effect)?;
            if documents.is_empty() {
                break;
            }
            build.content(documents)?;
        }
        build.metadata_complete = true;
        build.report.content_complete_ns = build.began.elapsed().as_nanos();
        build.observe(FourStepEvent::ContentComplete, None)?;
        // Step 3 completion alone never proves that provisional target names
        // decode. Only after the global content pass may an already structured
        // requested document answer through the ordinary canonical builder.
        for path in &loading.requested {
            if four_step::pending_at(build.store, Step::Structure, path)
                .map_err(store_effect)?
                .is_none()
                && build
                    .store
                    .begin_request()
                    .stored_document(path)
                    .map_err(store_effect)?
                    .is_some()
            {
                build.observe(FourStepEvent::GetReady(path.as_str()), Some(path.as_str()))?;
            }
        }
        loop {
            let documents = four_step::pending(build.store, Step::Structure, loading.bound)
                .map_err(store_effect)?;
            if documents.is_empty() {
                break;
            }
            build.structure(documents)?;
        }
        build.observe(FourStepEvent::StructureComplete, None)?;
        loop {
            let counters = four_step::health(build.store, loading.bound, declared.content_model())
                .map_err(store_effect)?;
            if counters.commits == 0 {
                break;
            }
            build.add("relations", counters);
        }
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
        let counters = four_step::finish(build.store).map_err(store_effect)?;
        build.add("relations", counters);
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
    began: Instant,
}

impl Build<'_> {
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
        self.loading
            .observer
            .at(
                event,
                &FourStepRead {
                    reader: self.reader,
                    model: self.declared.content_model(),
                    authority: self.authority,
                    ready_path,
                    metadata_complete: self.metadata_complete,
                },
            )
            .map_err(JobFailure::Environmental)
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
        for fact in walk(self.root, self.exclusions).map_err(effect)? {
            match fact.map_err(effect)? {
                norn_fs::WalkFact::File(file) => {
                    if let Ok(path) = document_path(file.path().as_path()) {
                        if is_markdown(file.path().as_path()) {
                            documents.push(path);
                        } else if path.is_vault_file() {
                            files.push(path);
                        }
                    }
                }
                norn_fs::WalkFact::Skipped(skip) => {
                    if !skip.reason().stands() {
                        self.changed();
                    }
                }
            }
            if documents.len() + files.len() >= self.loading.bound {
                let counters = four_step::names(
                    self.store,
                    &documents,
                    &files,
                    self.declared.content_model(),
                )
                .map_err(store_effect)?;

                self.add("names", counters);
                documents.clear();
                files.clear();
            }
        }
        if !documents.is_empty() || !files.is_empty() {
            let counters = four_step::names(
                self.store,
                &documents,
                &files,
                self.declared.content_model(),
            )
            .map_err(store_effect)?;

            self.add("names", counters);
        }
        // Refused spellings are replayed only as names, after every valid name
        // stands, so ordinary place-finding withholding sees the complete set.
        // This second walk never reads document bytes.
        let mut account = Account::default();
        let mut pending = Pending::new(
            self.store,
            self.loading.bound,
            self.root,
            self.exclusions,
            &mut account,
        )?;
        let mut commits = 0;
        let mut findings_written = 0;
        for fact in walk(self.root, self.exclusions).map_err(effect)? {
            match fact.map_err(effect)? {
                norn_fs::WalkFact::File(file) if is_markdown(file.path().as_path()) => {
                    if let Err(quarantine) = document_path(file.path().as_path()) {
                        pending.quarantine(file.path().as_path(), quarantine);
                        if pending.is_full() {
                            pending.counters = norn_store::DerivationCounters::default();
                            pending.flush()?;
                            let count = pending.counters.findings_written();
                            commits += u64::from(count > 0);
                            findings_written += count;
                        }
                    }
                }
                norn_fs::WalkFact::Skipped(skip) => {
                    pending.passed_over(skip.path().as_path(), skip.reason())
                }
                _ => {}
            }
        }
        if !pending.queued.is_empty() {
            pending.counters = norn_store::DerivationCounters::default();
            pending.flush()?;
            let count = pending.counters.findings_written();
            commits += u64::from(count > 0);
            findings_written += count;
        }
        drop(pending);
        // Normal Pending already records its evidence; include only the phase
        // receipt here to avoid charging those writes twice to JobEvidence.
        let counters = StepCounters {
            commits,
            findings_written,
            ..StepCounters::default()
        };
        for (name, count) in readings(&counters) {
            *self
                .report
                .steps
                .entry("names")
                .or_default()
                .entry(name)
                .or_default() += count;
        }
        self.ops
            .evidence
            .count_four_step_receipt("names", &counters);
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
            let Some(read) = self.read(&document.path)? else {
                self.changed();
                let counters =
                    four_step::forget(self.store, &document.path).map_err(store_effect)?;

                self.add("content", counters);
                continue;
            };
            files_read += 1;
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
                offsets.push(origin.expect("decoded content carries its origin").line as u64 - 1);
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
        for document in documents {
            let body = if self.loading.no_body {
                let read = self.read(&document.path)?;
                files_read += u64::from(read.is_some());
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
                document.body
            };
            let mut fact = DocumentFacts::new(
                document.path.clone(),
                document.content_hash,
                body,
                document.byte_length,
            );
            fact.body_offset = document.body_offset;
            crate::derivation::four_step_structure(
                &mut fact,
                norn_text::SourceSpan {
                    byte_offset: document.body_offset as usize,
                    line: document.body_line_offset as usize + 1,
                    column: 1,
                },
            );
            if self.declared.schema().tags().reports_undeclared() {
                let fm = self
                    .store
                    .begin_request()
                    .stored_facts(&document.path)
                    .map_err(store_effect)?
                    .expect("content row");
                let mut tags = fm.tags.into_iter().map(|tag| tag.fact).collect::<Vec<_>>();
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
        }
        if !facts.is_empty() {
            let counters = four_step::structure(
                self.store,
                &facts,
                self.declared.content_model(),
                self.loading.no_body,
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
        if !findings.is_empty() {
            let mut request = self.store.begin_request();
            request
                .apply_increment(
                    IncrementProvenance::Derived,
                    [],
                    &findings,
                    self.declared.content_model(),
                )
                .map_err(store_effect)?;
            count_increment_work(request.counters(), request.read_steps());
            let count = request.counters().findings_written();
            *self
                .report
                .steps
                .entry("structure")
                .or_default()
                .entry("commits")
                .or_default() += 1;
            *self
                .report
                .steps
                .entry("structure")
                .or_default()
                .entry("findings_written")
                .or_default() += count;
            self.ops.evidence.count_four_step_receipt(
                "structure",
                &StepCounters {
                    commits: 1,
                    findings_written: count,
                    ..StepCounters::default()
                },
            );
        }
        for fact in &facts {
            if self.loading.requested.contains(&fact.path) {
                self.report.requested_structure_ns.insert(
                    fact.path.as_str().to_owned(),
                    self.began.elapsed().as_nanos(),
                );
            }
            self.observe(
                FourStepEvent::Structured(fact.path.as_str()),
                Some(fact.path.as_str()),
            )?;
            if self.metadata_complete {
                self.observe(
                    FourStepEvent::GetReady(fact.path.as_str()),
                    Some(fact.path.as_str()),
                )?;
            }
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
        ("fact_rows_written", counters.fact_rows_written),
        ("links_judged", counters.links_judged),
        ("findings_written", counters.findings_written),
        ("findings_discarded", counters.findings_discarded),
        ("commits", counters.commits),
        ("files_read", counters.files_read),
    ])
}

impl crate::Host<ProductionEntryOps> {
    /// Committed phase counters from this host's lab builds.
    pub fn four_step_steps(&self) -> BTreeMap<&'static str, BTreeMap<&'static str, u64>> {
        self.ops().evidence.four_step_steps()
    }
}
