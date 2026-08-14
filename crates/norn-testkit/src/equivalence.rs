//! Whether two derived stores say the same thing about a vault.
//!
//! Two stores derived from one tree by two different routes — one attached and
//! healed, one built from zero — either hold the same answers or they do not,
//! and the difference is a defect in whichever route is wrong. This module is
//! how that question is asked: [`StoreProjection::read`] takes everything a
//! store holds that can change a later answer, and [`StoreProjection::compare`]
//! judges two of them.
//!
//! # What the projection carries, and what it drops
//!
//! It carries every **derived fact**: the document rows and their bodies, the
//! content hashes, byte lengths, body offsets and frontmatter diagnostic counts,
//! the frontmatter projection, the links, headings, block ids and tags, the
//! terms the full-text index holds, the pinned vault schema, and every finding —
//! findings at paths no document row stands at included, because those are
//! exactly the ones a keyed read cannot be asked for.
//!
//! It drops every **incidental** value, and each of the three is dropped for one
//! reason: nothing downstream can read it, and two stores that agree on every
//! answer disagree on it routinely.
//!
//! - **Row identifiers.** A row id is where a row landed in one file. Two
//!   stores that wrote the same documents in different orders hold different
//!   ones, and no answer is derived from them.
//! - **Write generations.** A generation orders the writes of one store. Two
//!   stores that took different numbers of writes to reach one state carry
//!   different generations for that state.
//! - **Timestamps.** `derived_at` and `recorded_at` are for a person reading a
//!   report, and two derivations of one vault happen at two instants by
//!   construction.
//!
//! # Two pillars are outside the projection, and each for its own reason
//!
//! - **Tombstones.** A death is a fact about one store's history rather than
//!   about the vault as it stands. A store that watched a document leave holds
//!   a tombstone for it; a store derived from zero over the same tree now
//!   records no death for a document it never saw, and the two agree about
//!   every answer either of them can give. Projecting deaths would therefore
//!   fail exactly the healed-against-rebuilt comparison this module exists to
//!   make. What holds a store's deaths to account is the per-store leg:
//!   [`assert_operationally_valid`] drains the pillar, which is where the
//!   closed vocabulary and the generation ordering each death against a later
//!   fact about the same path are read.
//! - **Document vectors.** No derivation writes `document_vectors`. The store
//!   accepts a vector through its own writer and no host job calls it, so the
//!   table is empty in every store this comparator reads and projecting it
//!   would compare one empty table against another. The embedding layer is what
//!   extends the projection to cover it, at the point where two derivations
//!   converging on the same vectors becomes a claim.
//!
//! # Equality is never vacuous here
//!
//! Two empty stores are equal, and reading that as evidence would green a
//! workload that derived nothing at all. Three things stand against it. The
//! projection is read through enumerators, so an empty answer means an empty
//! pillar rather than a read that asked about nothing. [`Population`] is carried
//! on every projection and reported by every comparison, so what was compared is
//! part of the verdict rather than something a reader has to go and check. And
//! [`StoreProjection::assert_holds`] states concrete rows a projection must hold,
//! so a case pairs its relative claim with an absolute one.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use norn_store::{
    BlockFact, DocumentPath, HeadingFact, IndexedTerm, LinkFact, PillarReport, Store, StoreError,
    StoredFinding, StoredPathOrder, StoredTombstone, TagFact, ddl,
};

/// How many rows one page of a drain asks for.
///
/// Well under the page bound the store accepts, so a projection of a large
/// vault is read in many bounded pages rather than one wide one: a reader that
/// asked for the largest page it could would be a working set that grows with
/// the vault.
///
/// Public because a case that puts a row exactly where a page ends has to spell
/// that position, and a case that spelled its own number would stop testing the
/// boundary the moment this one moved.
pub const PAGE: usize = 128;

/// Everything one store holds that can change a later answer.
///
/// Read it with [`StoreProjection::read`] and judge two of them with
/// [`StoreProjection::compare`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreProjection {
    documents: Vec<ProjectedDocument>,
    findings: Vec<ProjectedFinding>,
    terms: Vec<IndexedTerm>,
    vault_schema: Option<ProjectedSchema>,
}

/// One document row and everything derived from it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedDocument {
    pub path: String,
    pub content_hash: String,
    pub byte_length: u64,
    pub body_offset: u64,
    pub frontmatter: Option<String>,
    pub frontmatter_diagnostic_count: u32,
    pub body: String,
    pub links: Vec<LinkFact>,
    pub headings: Vec<HeadingFact>,
    pub blocks: Vec<BlockFact>,
    pub tags: Vec<TagFact>,
}

/// One finding, with the write generation it was recorded at left out.
///
/// The vault-schema fingerprint stays: it is not a generation but the key that
/// says which schema the finding was derived under, and two stores that agree
/// about a vault agree about that.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProjectedFinding {
    pub path: String,
    pub kind: String,
    pub severity: String,
    pub target: Option<String>,
    pub message: String,
    pub detail: Option<String>,
    pub span: Option<(u64, u64, u64)>,
    pub candidates: Vec<(String, String)>,
    pub candidates_total: u64,
    pub class_keys: BTreeSet<String>,
    pub vault_schema_fingerprint: String,
}

/// The pinned vault schema, with the generation it was pinned at left out.
///
/// The bytes and the fingerprint are two fields rather than one, because the
/// fingerprint is the store's own claim *about* the bytes and the comparison is
/// what cross-checks it: a pair that agreed on the fingerprint while holding
/// different bytes is exactly the drift a projection summarising the bytes by
/// their fingerprint would report nothing about.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedSchema {
    pub bytes: Vec<u8>,
    pub fingerprint: String,
}

/// How much a projection is standing on.
///
/// Every comparison reports one, so a verdict of "equal" is read beside what was
/// compared rather than on its own.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Population {
    pub documents: usize,
    pub facts: usize,
    pub findings: usize,
    pub indexed_terms: usize,
    pub vault_schema_pinned: bool,
}

impl Population {
    /// Whether this projection stands on nothing at all, which is the state two
    /// stores can be equal in while having derived nothing.
    pub fn is_empty(&self) -> bool {
        self.documents == 0
            && self.findings == 0
            && self.indexed_terms == 0
            && !self.vault_schema_pinned
    }
}

impl std::fmt::Display for Population {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} documents, {} derived fact rows, {} findings, {} indexed terms, {}",
            self.documents,
            self.facts,
            self.findings,
            self.indexed_terms,
            if self.vault_schema_pinned {
                "a pinned vault schema"
            } else {
                "no pinned vault schema"
            }
        )
    }
}

/// The first place two projections disagree.
///
/// It names one field of one subject, because that is what a reader has to go
/// and look at: a report that said only "unequal" would leave a failing case to
/// be diagnosed by printing both projections and reading them side by side.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Divergence {
    /// The field, spelled as the path to it — `document[docs/a.md].content_hash`.
    pub field: String,
    /// What the left projection says, and `(absent)` where it holds no such
    /// field at all.
    pub left: String,
    /// The same for the right.
    pub right: String,
}

impl std::fmt::Display for Divergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`{}`: the first store says {} and the second says {}",
            self.field, self.left, self.right
        )
    }
}

/// What comparing two projections concluded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Comparison {
    /// The first field the two disagree about, and nothing where they agree.
    pub divergence: Option<Divergence>,
    pub left: Population,
    pub right: Population,
}

impl Comparison {
    pub fn is_equal(&self) -> bool {
        self.divergence.is_none()
    }
}

impl StoreProjection {
    /// Read everything the store holds that can change a later answer.
    ///
    /// Every pillar is drained through an enumerator rather than sampled by key,
    /// so a pillar that comes back empty is an empty pillar and never a read
    /// that asked about nothing.
    pub fn read(store: &mut Store) -> Result<Self, StoreError> {
        let mut projection = StoreProjection {
            documents: Vec::new(),
            findings: Vec::new(),
            terms: Vec::new(),
            vault_schema: None,
        };
        let mut request = store.begin_request();

        let mut after: Option<DocumentPath> = None;
        loop {
            let page = request.stored_documents_after_ordered(
                after.as_ref(),
                PAGE,
                StoredPathOrder::Sensitive,
            )?;
            let Some(last) = page.last() else { break };
            after = Some(last.path.clone());
            let paths: Vec<DocumentPath> = page.into_iter().map(|row| row.path).collect();
            for path in paths {
                let facts = request
                    .stored_facts(&path)?
                    .expect("a page named a document row the store holds");
                projection.documents.push(ProjectedDocument {
                    path: facts.document.path.as_str().to_string(),
                    content_hash: facts.document.content_hash,
                    byte_length: facts.document.byte_length,
                    body_offset: facts.document.body_offset,
                    frontmatter: facts.document.frontmatter,
                    frontmatter_diagnostic_count: facts.document.frontmatter_diagnostic_count,
                    body: facts.body,
                    links: facts.links,
                    headings: facts.headings,
                    blocks: facts.blocks,
                    tags: facts.tags,
                });
            }
        }

        let mut cursor = None;
        loop {
            let page = request.stored_findings_after(cursor, PAGE)?;
            let Some((last, _)) = page.last() else { break };
            cursor = Some(*last);
            projection.findings.extend(
                page.into_iter()
                    .map(|(_, finding)| project_finding(finding)),
            );
        }
        // A finding carries no key that survives being written to a second
        // store, so the order two stores hand them back in is the order each
        // wrote them. Sorting by the finding's own content is what makes the
        // two comparable at all.
        projection.findings.sort();

        let mut term: Option<String> = None;
        loop {
            let page = request.indexed_terms_after(term.as_deref(), PAGE)?;
            let Some(last) = page.last() else { break };
            term = Some(last.term.clone());
            projection.terms.extend(page);
        }

        projection.vault_schema = request.vault_schema_pin()?.map(|pin| ProjectedSchema {
            bytes: pin.bytes,
            fingerprint: pin.fingerprint,
        });
        Ok(projection)
    }

    pub fn documents(&self) -> &[ProjectedDocument] {
        &self.documents
    }

    pub fn findings(&self) -> &[ProjectedFinding] {
        &self.findings
    }

    pub fn indexed_terms(&self) -> &[IndexedTerm] {
        &self.terms
    }

    pub fn vault_schema(&self) -> Option<&ProjectedSchema> {
        self.vault_schema.as_ref()
    }

    /// The document stored at `path`, and nothing where no row stands there.
    pub fn document(&self, path: &str) -> Option<&ProjectedDocument> {
        self.documents.iter().find(|document| document.path == path)
    }

    /// How much this projection stands on.
    pub fn population(&self) -> Population {
        Population {
            documents: self.documents.len(),
            facts: self
                .documents
                .iter()
                .map(|document| {
                    document.links.len()
                        + document.headings.len()
                        + document.blocks.len()
                        + document.tags.len()
                })
                .sum(),
            findings: self.findings.len(),
            indexed_terms: self.terms.len(),
            vault_schema_pinned: self.vault_schema.is_some(),
        }
    }

    /// Judge this projection against another, naming the first field they
    /// disagree about.
    pub fn compare(&self, other: &StoreProjection) -> Comparison {
        Comparison {
            divergence: first_divergence(&self.entries(), &other.entries()),
            left: self.population(),
            right: other.population(),
        }
    }

    /// **The equivalence assertion.** Two stores hold the same derived facts.
    ///
    /// The population is printed on success as well as on failure, because a
    /// comparison of two empty projections passes and says nothing.
    pub fn assert_equivalent(&self, other: &StoreProjection, subject: &str) {
        let comparison = self.compare(other);
        assert!(
            comparison.is_equal(),
            "{subject}: the two stores are not equivalent. {}\nthe first holds {}\nthe second \
             holds {}",
            comparison
                .divergence
                .expect("an unequal comparison names a divergence"),
            comparison.left,
            comparison.right
        );
    }

    /// **The absolute assertion.** This projection holds a document at each of
    /// these paths, with the body each names.
    ///
    /// A relative claim is only as strong as what both sides derived, so a case
    /// that asserts equivalence over a workload states some of what that
    /// workload put there too: two stores that both omitted a document are
    /// equivalent and both wrong.
    pub fn assert_holds(&self, subject: &str, expected: &[(&str, &str)]) {
        for (path, body) in expected {
            let held = self.document(path).unwrap_or_else(|| {
                panic!(
                    "{subject}: no document row stands at `{path}`, and the projection holds {}",
                    self.population()
                )
            });
            assert_eq!(
                &held.body, body,
                "{subject}: the document at `{path}` holds another body"
            );
        }
    }

    /// **The non-vacuity floor.** This projection stands on at least this much.
    ///
    /// Each number is a floor rather than an equality, so a case states what its
    /// workload must at least have derived without restating the workload.
    pub fn assert_population_at_least(&self, subject: &str, floor: Population) {
        let held = self.population();
        let short = [
            ("documents", held.documents, floor.documents),
            ("derived fact rows", held.facts, floor.facts),
            ("findings", held.findings, floor.findings),
            ("indexed terms", held.indexed_terms, floor.indexed_terms),
        ]
        .into_iter()
        .find(|(_, held, floor)| held < floor);
        assert!(
            short.is_none(),
            "{subject}: the projection holds {} of {} and the floor is {}; it holds {held}",
            short.expect("a shortfall").1,
            short.expect("a shortfall").0,
            short.expect("a shortfall").2,
        );
        assert!(
            !floor.vault_schema_pinned || held.vault_schema_pinned,
            "{subject}: the projection carries no pinned vault schema; it holds {held}"
        );
    }

    /// The projection as a map from field to value.
    ///
    /// This is the form the comparison is made in, and it exists so that a
    /// disagreement can be reported as one named field rather than as two whole
    /// projections a reader has to diff by eye. **Every field names itself**:
    /// a document's fields carry its path, a finding's carry its subject, a
    /// term's carry the term. So a fact one store holds and the other does not
    /// is a field with nothing opposite it, rather than a shift that renames
    /// every field after it.
    fn entries(&self) -> BTreeMap<String, String> {
        let mut entries = Vec::new();
        for document in &self.documents {
            let at = format!("document[{}]", document.path);
            entries.push((format!("{at}.content_hash"), quoted(&document.content_hash)));
            entries.push((
                format!("{at}.byte_length"),
                document.byte_length.to_string(),
            ));
            entries.push((
                format!("{at}.body_offset"),
                document.body_offset.to_string(),
            ));
            entries.push((
                format!("{at}.frontmatter"),
                document
                    .frontmatter
                    .as_deref()
                    .map_or_else(|| "(none)".to_string(), quoted),
            ));
            entries.push((
                format!("{at}.frontmatter_diagnostic_count"),
                document.frontmatter_diagnostic_count.to_string(),
            ));
            entries.push((format!("{at}.body"), quoted(&document.body)));
            push_indexed(&mut entries, &at, "link", &document.links);
            push_indexed(&mut entries, &at, "heading", &document.headings);
            push_indexed(&mut entries, &at, "block", &document.blocks);
            push_indexed(&mut entries, &at, "tag", &document.tags);
        }
        // A finding has no key of its own that survives being written to a
        // second store, so its field is its subject and its position among the
        // findings about that subject. Two findings that differ only in where
        // they sort therefore report as two fields rather than as one shifted
        // list.
        let mut at_subject: BTreeMap<&str, usize> = BTreeMap::new();
        for finding in &self.findings {
            let ordinal = at_subject.entry(finding.path.as_str()).or_default();
            entries.push((
                format!("finding[{}][{ordinal}]", finding.path),
                format!("{finding:?}"),
            ));
            *ordinal += 1;
        }
        for term in &self.terms {
            entries.push((
                format!("indexed term[{}]", term.term),
                format!(
                    "{} documents, {} occurrences",
                    term.documents, term.occurrences
                ),
            ));
        }
        // The schema's bytes are rendered as their own escaped content, so the
        // comparison reads the bytes themselves. A length summarises them and a
        // digest computed from them here would be a second spelling of the
        // fingerprint the store recorded — which is the field beside them, and
        // the one this pair exists to cross-check.
        //
        // Both halves of the pair are quoted, which is what keeps a pinned
        // schema apart from an absent one: a rendering left bare could spell the
        // absent marker with content, and quoting puts the marker outside the
        // range every present value renders into.
        const NONE: &str = "(none)";
        let (bytes, fingerprint) = self.vault_schema.as_ref().map_or_else(
            || (NONE.to_string(), NONE.to_string()),
            |schema| {
                (
                    quoted(&schema.bytes.escape_ascii().to_string()),
                    quoted(&schema.fingerprint),
                )
            },
        );
        entries.push(("vault schema.bytes".to_string(), bytes));
        entries.push(("vault schema.fingerprint".to_string(), fingerprint));
        let rendered: BTreeMap<String, String> = entries.iter().cloned().collect();
        assert_eq!(
            rendered.len(),
            entries.len(),
            "two facts rendered as one field, so a comparison would judge one of them and \
             report nothing about the other"
        );
        rendered
    }
}

/// **The operational-validity leg.** One store is internally sound, whatever any
/// other store says.
///
/// Equivalence is a claim about two stores together, and two stores can be
/// equivalent and both damaged. These are the claims each store answers on its
/// own:
///
/// - It passes the store's own integrity verification — the pages, the foreign
///   keys, the full-text index against the column it indexes, the frontmatter
///   projection against being JSON, and the closed vocabularies.
/// - The store schema it records is the one this build writes, and the digest of
///   the schema it actually holds is the digest it recorded holding.
/// - Every tombstone reads back: a provenance outside the closed vocabulary is
///   what the enumerator refuses rather than returns, so a drain that completes
///   is the vocabulary holding over every row rather than over the ones somebody
///   thought to ask about. Each death was recorded at a generation the store
///   took, which is what orders it against a late fact about the same path.
/// - The migration ledger is empty, which is what the pre-release build's
///   evolution path is: a store schema change is a rebuild from zero and
///   consumes no version number, so a row here would be a migration nothing
///   applied.
/// - Every stored suffix key is the key its own path produces. The column is a
///   derived one the resolution ladder ranges over, and no read that answers a
///   caller's question compares it with the path beside it, so a row whose key
///   drifted answers a suffix probe it does not belong to and stands outside
///   the range of the one it does. The store's suffix-key enumerator is what
///   hands the two back as a pair, and this leg drains it. The claim is about
///   one store rather than about two — the key is a pure function of the path
///   both stores hold — which is why it is asked here and not in the
///   projection.
pub fn assert_operationally_valid(store: &mut Store, subject: &str) {
    store.verify_integrity().unwrap_or_else(|problem| {
        panic!("{subject}: the store is not internally consistent: {problem}")
    });

    let recorded = store.recorded_store_schema().unwrap_or_else(|problem| {
        panic!("{subject}: reading the recorded store schema: {problem}")
    });
    assert_eq!(
        recorded.version,
        Some(ddl::STORE_SCHEMA_VERSION),
        "{subject}: the store records another store schema version"
    );
    assert_eq!(
        recorded.ddl_fingerprint,
        Some(ddl::fingerprint()),
        "{subject}: the store records a DDL fingerprint this build did not write"
    );
    let held = store
        .schema_digest()
        .unwrap_or_else(|problem| panic!("{subject}: digesting the schema: {problem}"));
    assert_eq!(
        recorded.schema_digest,
        Some(held),
        "{subject}: the schema the store holds is not the schema it recorded holding"
    );

    let pillars = pillar_report(store)
        .unwrap_or_else(|problem| panic!("{subject}: reading the pillar report: {problem}"));
    assert_eq!(
        pillars.migrations_applied, 0,
        "{subject}: the migration ledger carries {} rows, and a pre-release store schema change \
         is a rebuild from zero rather than a migration",
        pillars.migrations_applied
    );

    let deaths = tombstones(store)
        .unwrap_or_else(|problem| panic!("{subject}: draining the tombstone pillar: {problem}"));
    assert_eq!(
        deaths.len() as u64,
        pillars.tombstones,
        "{subject}: the pillar holds {} tombstones and the drain reached {}",
        pillars.tombstones,
        deaths.len()
    );
    for death in &deaths {
        assert!(
            death.generation > 0,
            "{subject}: the death recorded at `{}` carries no write generation, so nothing \
             orders it against a later fact about the same path",
            death.path.as_str()
        );
    }

    for_each_stored_suffix_key(store, |path, stored| {
        assert_eq!(
            stored,
            path.suffix_key(),
            "{subject}: the row at `{}` holds a suffix key its own path does not produce",
            path.as_str()
        );
    })
    .unwrap_or_else(|problem| panic!("{subject}: draining the stored suffix keys: {problem}"));
}

/// Hand every row's stored suffix key over beside its path, a bounded page at a
/// time.
///
/// The pair comes off the store's own suffix-key enumerator rather than off a
/// document page, so the recompute reaches every row without putting the key
/// column on the readers that page documents for their facts.
fn for_each_stored_suffix_key(
    store: &mut Store,
    mut visit: impl FnMut(&DocumentPath, &str),
) -> Result<(), StoreError> {
    let request = store.begin_request();
    let mut after: Option<DocumentPath> = None;
    loop {
        let page = request.suffix_keys_after(after.as_ref(), PAGE)?;
        let Some((last, _)) = page.last() else {
            return Ok(());
        };
        after = Some(last.clone());
        for (path, stored) in &page {
            visit(path, stored);
        }
    }
}

/// Every tombstone the store holds, drained a bounded page at a time.
pub fn tombstones(store: &mut Store) -> Result<Vec<StoredTombstone>, StoreError> {
    let request = store.begin_request();
    let mut drained = Vec::new();
    let mut after: Option<DocumentPath> = None;
    loop {
        let page = request.stored_tombstones_after(after.as_ref(), PAGE)?;
        let Some(last) = page.last() else {
            return Ok(drained);
        };
        after = Some(last.path.clone());
        drained.extend(page);
    }
}

fn pillar_report(store: &mut Store) -> Result<PillarReport, StoreError> {
    store.begin_request().pillars()
}

fn project_finding(finding: StoredFinding) -> ProjectedFinding {
    ProjectedFinding {
        path: finding.path.as_str().to_string(),
        kind: finding.kind,
        severity: finding.severity,
        target: finding.target,
        message: finding.message,
        detail: finding.detail,
        span: finding
            .span
            .map(|span| (span.line, span.column, span.byte_offset)),
        candidates: finding
            .candidates
            .into_iter()
            .map(|candidate| (candidate.path.as_str().to_string(), candidate.suffix))
            .collect(),
        candidates_total: finding.candidates_total,
        class_keys: finding
            .class_keys
            .iter()
            .map(|key| key.as_str().to_string())
            .collect(),
        vault_schema_fingerprint: finding.vault_schema_fingerprint,
    }
}

/// Render a document's ordered fact rows, ordinal included: the order is what
/// the text layer emitted and is itself a derived fact.
fn push_indexed<T: std::fmt::Debug>(
    entries: &mut Vec<(String, String)>,
    at: &str,
    name: &str,
    rows: &[T],
) {
    entries.push((format!("{at}.{name} count"), rows.len().to_string()));
    for (ordinal, row) in rows.iter().enumerate() {
        entries.push((format!("{at}.{name}[{ordinal}]"), format!("{row:?}")));
    }
}

/// The first field the two disagree about, reading every field either of them
/// carries.
///
/// A field one side carries and the other does not is a disagreement about that
/// field, reported with `(absent)` opposite it. Comparing by field rather than
/// by position is what keeps a fact only one store holds from renaming every
/// fact after it: one document more on one side is one absent field, not a whole
/// projection that looks different from there on.
fn first_divergence(
    left: &BTreeMap<String, String>,
    right: &BTreeMap<String, String>,
) -> Option<Divergence> {
    const ABSENT: &str = "(absent)";
    left.keys()
        .chain(right.keys())
        .collect::<BTreeSet<&String>>()
        .into_iter()
        .find_map(|field| {
            let (one, two) = (left.get(field), right.get(field));
            (one != two).then(|| Divergence {
                field: field.clone(),
                left: one.map_or(ABSENT, String::as_str).to_string(),
                right: two.map_or(ABSENT, String::as_str).to_string(),
            })
        })
}

/// A value rendered so that whitespace and emptiness are visible in a failure.
fn quoted(value: &str) -> String {
    let mut rendered = String::with_capacity(value.len() + 2);
    write!(rendered, "{value:?}").expect("writing to a string");
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projection() -> StoreProjection {
        StoreProjection {
            documents: vec![ProjectedDocument {
                path: "docs/a.md".to_string(),
                content_hash: "hash-1".to_string(),
                byte_length: 7,
                body_offset: 0,
                frontmatter: None,
                frontmatter_diagnostic_count: 0,
                body: "a body\n".to_string(),
                links: Vec::new(),
                headings: Vec::new(),
                blocks: Vec::new(),
                tags: Vec::new(),
            }],
            findings: Vec::new(),
            terms: vec![IndexedTerm {
                term: "body".to_string(),
                documents: 1,
                occurrences: 1,
            }],
            vault_schema: None,
        }
    }

    #[test]
    fn two_readings_of_one_shape_are_equal() {
        let comparison = projection().compare(&projection());
        assert!(comparison.is_equal(), "{comparison:?}");
        assert_eq!(comparison.left, comparison.right);
    }

    #[test]
    fn a_changed_field_is_reported_by_name() {
        let mut other = projection();
        other.documents[0].content_hash = "hash-2".to_string();
        let divergence = projection()
            .compare(&other)
            .divergence
            .expect("a changed field diverges");
        assert_eq!(divergence.field, "document[docs/a.md].content_hash");
        assert!(divergence.left.contains("hash-1"), "{divergence}");
        assert!(divergence.right.contains("hash-2"), "{divergence}");
    }

    /// A document one store derived and the other did not is a named field with
    /// nothing opposite it, rather than a count that came out different.
    #[test]
    fn a_document_only_one_store_holds_is_reported_at_its_own_field() {
        let mut other = projection();
        other.documents.clear();
        let divergence = projection()
            .compare(&other)
            .divergence
            .expect("a missing document diverges");
        assert!(
            divergence.field.starts_with("document[docs/a.md]"),
            "{divergence}"
        );
    }

    #[test]
    fn a_comparison_reports_what_it_stood_on() {
        let comparison = projection().compare(&projection());
        assert_eq!(comparison.left.documents, 1);
        assert_eq!(comparison.left.indexed_terms, 1);
        assert!(!comparison.left.is_empty());
        assert!(Population::default().is_empty());
    }

    #[test]
    #[should_panic(expected = "no document row stands at `docs/missing.md`")]
    fn an_absolute_assertion_fails_where_the_row_is_not_there() {
        projection().assert_holds("a fixture floor", &[("docs/missing.md", "a body\n")]);
    }

    #[test]
    #[should_panic(expected = "the floor is 2")]
    fn a_population_floor_fails_a_projection_that_stands_on_less() {
        projection().assert_population_at_least(
            "a fixture floor",
            Population {
                documents: 2,
                ..Population::default()
            },
        );
    }

    #[test]
    fn a_population_floor_passes_what_meets_it() {
        projection().assert_holds("a fixture floor", &[("docs/a.md", "a body\n")]);
        projection().assert_population_at_least(
            "a fixture floor",
            Population {
                documents: 1,
                indexed_terms: 1,
                ..Population::default()
            },
        );
    }
}
