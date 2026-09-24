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
//! content hashes and the sub-fingerprints beside them, byte lengths, body
//! offsets and frontmatter diagnostic counts, the frontmatter projection, the
//! links, headings, block ids and tags, the field rows with their typed sort
//! keys, the terms the full-text index holds, the
//! pinned vault schema, and every finding — findings at paths no document row
//! stands at included, because those are exactly the ones a keyed read cannot be
//! asked for.
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
//!   make.
//!
//!   **What stands in its place is two claims, and only the second is about
//!   which deaths are there.** [`assert_operationally_valid`] is the per-store
//!   leg, and what it reads is the pillar's *shape*: every row comes back
//!   through the enumerator, so the closed vocabulary holds over all of them
//!   rather than over the ones somebody asked about, and each death carries a
//!   write generation ordering it against a later fact about the same path. It
//!   says nothing about membership — a store that recorded no deaths at all
//!   passes it, agreeing with a count of zero. **Membership is the churning
//!   suite's**, because only a suite that changed the tree knows which places
//!   stopped deriving: it reads that off its own censuses and asks
//!   [`tombstones`] for a row at each, which is why that drain is public here
//!   and outside the projection.
//! - **Document vectors.** The store holds no vector state: embeddings are
//!   lane-2 engine state in a sidecar database ([ADR
//!   0021](../../../docs/decisions/0021-derived-indexes-split-into-two-lanes.md)),
//!   outside the lane-1 projection this comparator reads. Two derivations
//!   converging on the same vectors is the engine's own convergence bar, judged
//!   against the sidecar rather than here.
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
//!
//! # One store's rows, as one number
//!
//! [`DerivedRows`] is the same projection taken for one store on its own, with
//! each row's stored suffix keys beside it, and digested: the number a pinned
//! corpus derived from zero is held to, so a change to what derivation writes
//! for unchanged input cannot land without being seen. The suffix keys are in
//! it and not in the projection because the projection compares two stores
//! over one tree, where a key is a pure function of a path both hold; the
//! digest compares one build's derivation with another's, where the function
//! itself is what may have moved.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use norn_fixtures::digest::{Sha256, hex};
use norn_store::{
    BlockFact, DocumentPath, FieldRow, FieldRows, FindingCursor, HeadingFact, IndexedTerm,
    LinkFact, PillarReport, Span, Store, StoreError, StoredFinding, StoredPathOrder,
    StoredSuffixKeys, StoredTombstone, TagFact, ddl,
};
use norn_wire::{FindingKind, FindingScope};

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
    /// The sub-fingerprint of the body, as the change feed projects it. A
    /// derived fact like every other field here: two stores that agree about a
    /// document agree about what its body hashes to, whichever route derived
    /// them.
    pub body_hash: String,
    /// The sub-fingerprint of the frontmatter projection, and `None` where there
    /// is no projection to hash.
    pub frontmatter_projection_hash: Option<String>,
    pub byte_length: u64,
    pub body_offset: u64,
    pub frontmatter: Option<String>,
    pub frontmatter_diagnostic_count: u32,
    pub body: String,
    pub links: Vec<LinkFact>,
    pub headings: Vec<HeadingFact>,
    pub blocks: Vec<BlockFact>,
    pub tags: Vec<TagFact>,
    /// The field rows, typed half included: a store that healed under a
    /// re-pinned schema and one built from zero under it agree about every
    /// typed value only if the heal refilled what the pin cleared.
    pub fields: FieldRows,
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
///
/// Serializable because [`crate::fidelity`] is what a run's comparisons are
/// recorded through, and a population is half of what a reading carries.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
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
///
/// Serializable for the reason [`Population`] is: a divergence is the other half
/// of a [`crate::fidelity`] reading, and naming one field is what makes a
/// history of them worth scanning.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
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

    /// **The equivalence assertion, over a comparison already taken.**
    ///
    /// A caller that records a verdict before asserting on it holds the
    /// comparison, and taking a second one to assert against would drain both
    /// projections again and — worse — assert on a verdict other than the one it
    /// recorded. The two readings would agree, because a projection is a value;
    /// what would not be visible is that they are two readings at all.
    ///
    /// The population is printed on success as well as on failure, because a
    /// comparison of two empty projections passes and says nothing.
    pub fn assert_equal(&self, subject: &str) {
        assert!(
            self.is_equal(),
            "{subject}: the two stores are not equivalent. {}\nthe first holds {}\nthe second \
             holds {}",
            self.divergence
                .as_ref()
                .expect("an unequal comparison names a divergence"),
            self.left,
            self.right
        );
    }
}

impl StoreProjection {
    /// Read everything the store holds that can change a later answer.
    ///
    /// Every pillar is drained through an enumerator rather than sampled by key,
    /// so a pillar that comes back empty is an empty pillar and never a read
    /// that asked about nothing.
    pub fn read(store: &mut Store) -> Result<Self, StoreError> {
        Self::read_in(store, FindingOrder::Content)
    }

    /// [`StoreProjection::read`], holding the findings in `order`.
    fn read_in(store: &mut Store, order: FindingOrder) -> Result<Self, StoreError> {
        let mut projection = StoreProjection {
            documents: Vec::new(),
            findings: Vec::new(),
            terms: Vec::new(),
            vault_schema: None,
        };
        let mut request = store.begin_request();

        // The sub-fingerprints are read off the change feed rather than off the
        // document row, because that is the only reader that hands them out:
        // carrying two hex strings on every reader of a document page for the
        // sake of one comparator is the second home the schema refuses. The
        // whole feed is drained first and keyed by path, so each document takes
        // its pair without a read of its own. The positions the drain hands back
        // are dropped — a cursor is one store's, and a projection compares two.
        let mut sub_fingerprints: BTreeMap<String, (String, Option<String>)> = BTreeMap::new();
        let mut position = None;
        loop {
            let page = request.changed_documents_after(position.as_ref(), PAGE)?;
            let Some((last, _)) = page.last() else { break };
            // A drain ends because its cursor moves. One that stopped moving
            // would read the same page forever, so it is named here rather than
            // met as a run that never returns.
            assert!(
                position.as_ref().is_none_or(|held| held < last),
                "the change feed handed back a page that did not advance the cursor"
            );
            position = Some(last.clone());
            for (_, fed) in page {
                sub_fingerprints.insert(
                    fed.path.as_str().to_string(),
                    (fed.body_hash, fed.frontmatter_projection_hash),
                );
            }
        }

        let mut after: Option<DocumentPath> = None;
        loop {
            let page = request.stored_documents_after_ordered(
                after.as_ref(),
                PAGE,
                StoredPathOrder::Sensitive,
            )?;
            let Some(last) = page.last() else { break };
            assert!(
                after.as_ref().is_none_or(|held| held < &last.path),
                "the ordered document page did not advance the cursor"
            );
            after = Some(last.path.clone());
            let paths: Vec<DocumentPath> = page.into_iter().map(|row| row.path).collect();
            for path in paths {
                let facts = request
                    .stored_facts(&path)?
                    .expect("a page named a document row the store holds");
                let (body_hash, frontmatter_projection_hash) = sub_fingerprints
                    .remove(path.as_str())
                    .expect("the change feed reaches every document row the store holds");
                projection.documents.push(ProjectedDocument {
                    path: facts.document.path.as_str().to_string(),
                    content_hash: facts.document.content_hash,
                    body_hash,
                    frontmatter_projection_hash,
                    byte_length: facts.document.byte_length,
                    body_offset: facts.document.body_offset,
                    frontmatter: facts.document.frontmatter,
                    frontmatter_diagnostic_count: facts.document.frontmatter_diagnostic_count,
                    body: facts.body,
                    links: facts.links,
                    headings: facts.headings,
                    blocks: facts.blocks,
                    tags: facts.tags,
                    fields: facts.fields,
                });
            }
        }
        assert!(
            sub_fingerprints.is_empty(),
            "the change feed reached rows the ordered document page did not: {:?}",
            sub_fingerprints.keys().collect::<Vec<&String>>()
        );

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
        if order == FindingOrder::Content {
            projection.findings.sort();
        }

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

    /// The findings, in content order for a projection [`StoreProjection::read`]
    /// took and in row-key order for one [`DerivedRows`] holds.
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
    /// A caller that also records the verdict takes the comparison itself and
    /// asserts through [`Comparison::assert_equal`], so what it recorded and
    /// what it asserted on are one value.
    pub fn assert_equivalent(&self, other: &StoreProjection, subject: &str) {
        self.compare(other).assert_equal(subject);
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
    ///
    /// **A row is rendered column by column, as the database holds it**: each
    /// field ends in the name of the column it reads, and holds that column's
    /// value — text quoted, an integer or a flag as its digits, a closed
    /// vocabulary as the word the column stores, and [`NULL`] where the column
    /// holds none. So the rendering is a function of the stored rows alone:
    /// renaming a Rust field or variant that carries a column moves nothing
    /// here, and a changed stored value moves exactly the field that holds it.
    fn entries(&self) -> BTreeMap<String, String> {
        let mut entries = Vec::new();
        for document in &self.documents {
            let at = format!("document[{}]", document.path);
            entries.push((format!("{at}.content_hash"), quoted(&document.content_hash)));
            entries.push((format!("{at}.body_hash"), quoted(&document.body_hash)));
            entries.push((
                format!("{at}.frontmatter_projection_hash"),
                optional_text(document.frontmatter_projection_hash.as_deref()),
            ));
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
                optional_text(document.frontmatter.as_deref()),
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
            push_indexed(&mut entries, &at, "field", document.fields.rows());
        }
        // A finding has no key of its own that survives being written to a
        // second store, so its field is its subject and its position among the
        // findings about that subject, in the order this projection holds
        // them. Two findings that differ only in where they sort therefore
        // report as two fields rather than as one shifted list.
        let mut at_subject: BTreeMap<&str, usize> = BTreeMap::new();
        for finding in &self.findings {
            let ordinal = at_subject.entry(finding.path.as_str()).or_default();
            let at = format!("finding[{}][{ordinal}]", finding.path);
            push_columns(&mut entries, &at, finding);
            // The candidate head by the rank each row is stored at, and the
            // class memberships in the key order they are read in.
            entries.push((
                format!("{at}.candidate count"),
                finding.candidates.len().to_string(),
            ));
            for (rank, (path, suffix)) in finding.candidates.iter().enumerate() {
                entries.push((format!("{at}.candidate[{rank}].path"), quoted(path)));
                entries.push((format!("{at}.candidate[{rank}].suffix"), quoted(suffix)));
            }
            entries.push((
                format!("{at}.class_key count"),
                finding.class_keys.len().to_string(),
            ));
            for (index, key) in finding.class_keys.iter().enumerate() {
                entries.push((format!("{at}.class_key[{index}]"), quoted(key)));
            }
            *ordinal += 1;
        }
        for term in &self.terms {
            // The columns the full-text vocabulary reports a term's counts in.
            let at = format!("indexed term[{}]", term.term);
            entries.push((format!("{at}.doc"), term.documents.to_string()));
            entries.push((format!("{at}.cnt"), term.occurrences.to_string()));
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
        let (bytes, fingerprint) = self.vault_schema.as_ref().map_or_else(
            || (NULL.to_string(), NULL.to_string()),
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

/// The order a projection holds its findings in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FindingOrder {
    /// Sorted by each finding's own content: what two stores that wrote their
    /// findings in different orders are compared in.
    Content,
    /// The order the rows are keyed in, which is the order they were written
    /// in. A reader that pages a subject's findings reads them in this order
    /// among themselves, so one derivation's output includes it.
    Stored,
}

/// Every derived row one store holds, rendered field by field in field order.
///
/// Read it with [`DerivedRows::read`]. It carries what [`StoreProjection`]
/// carries and drops what it drops — row identifiers, write generations and
/// timestamps, none of which is a function of the vault — plus each document
/// row's stored suffix keys, raw and folded. Tombstones stay out for the
/// projection's reason: a death is one store's history, and a store derived
/// from zero records none.
///
/// **Every ordered read is rendered in the order a reader observes.** The
/// links, headings, blocks and tags carry their stored ordinal, the field rows
/// their key and ordinal, a finding's candidates their rank and its classes
/// their key, and every one of those is a column the row holds. A finding
/// carries no such column: validate pages a subject's findings by row key, and
/// find's head reads them by kind and then row key, so the order one subject's
/// findings were written in reaches an answer. Each subject's findings are
/// therefore rendered in row-key order, by their rank among that subject's
/// findings; the absolute keys, and the interleaving of two subjects, reach no
/// answer and stay out.
#[derive(Clone, Debug)]
pub struct DerivedRows {
    projection: StoreProjection,
    fields: BTreeMap<String, String>,
}

impl DerivedRows {
    /// Read every derived row `store` holds.
    pub fn read(store: &mut Store) -> Result<Self, StoreError> {
        let projection = StoreProjection::read_in(store, FindingOrder::Stored)?;
        let mut fields = projection.entries();
        let mut suffix_keys = Vec::new();
        for_each_stored_suffix_key(store, |stored| {
            let at = format!("document[{}]", stored.path.as_str());
            suffix_keys.push((format!("{at}.suffix_key"), quoted(&stored.raw)));
            suffix_keys.push((format!("{at}.folded_suffix_key"), quoted(&stored.folded)));
        })?;
        for (field, value) in suffix_keys {
            let collided = fields.insert(field, value);
            assert!(
                collided.is_none(),
                "a suffix key rendered as a field the projection already carries"
            );
        }
        Ok(DerivedRows { projection, fields })
    }

    /// The projection the rows were read through, for the claims a case makes
    /// about what the rows hold.
    pub fn projection(&self) -> &StoreProjection {
        &self.projection
    }

    /// The rows, one field to a value, in field order.
    pub fn fields(&self) -> &BTreeMap<String, String> {
        &self.fields
    }

    /// SHA-256 over the rows of every vault in `vaults`, in name order, as 64
    /// lowercase hex digits.
    ///
    /// A corpus spans more than one vault where what it exercises is a
    /// declaration a vault makes once — its stance on an undeclared tag — so
    /// the digest is taken over them together and each is named in it. The
    /// count of vaults leads, then each vault's name, the count of its fields,
    /// and every field and value; every one of them is absorbed behind its own
    /// length, so no two different sets of rows run together into the same
    /// bytes whatever their text holds. Nothing in it depends on where a store
    /// sits on disk: the fields are vault-relative and sorted, and every value
    /// is text.
    pub fn digest(vaults: &BTreeMap<&str, DerivedRows>) -> String {
        let mut hasher = Sha256::new();
        hasher.update_framed(&(vaults.len() as u64).to_be_bytes());
        for (name, rows) in vaults {
            hasher.update_framed(name.as_bytes());
            hasher.update_framed(&(rows.fields.len() as u64).to_be_bytes());
            for (field, value) in &rows.fields {
                hasher.update_framed(field.as_bytes());
                hasher.update_framed(value.as_bytes());
            }
        }
        hex(&hasher.finish())
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
///   **Which deaths stand here is not this leg's question**: a store holding
///   none passes, and what says the deaths a churn produced were recorded is the
///   suite that produced them — see this module's own ruling on the pillar.
/// - The migration ledger is empty, which is what the pre-release build's
///   evolution path is: a store schema change is a rebuild from zero and
///   consumes no version number, so a row here would be a migration nothing
///   applied.
/// - **Every finding stands where its kind says it may.** A finding's kind
///   decides its scope, and the scope is a claim about the row beside it: a
///   place-scoped finding — the three cause classes nothing is derivable from —
///   is withheld while a document row stands at its subject, and a
///   document-scoped finding — a frontmatter block nothing read — stands beside
///   the row it is about and is meaningless without one. So the pairing is
///   two-directional and this leg reads it both ways, over every finding the
///   store holds rather than over the ones a case thought to ask about. See
///   [ADR 0023]; the vocabulary is `norn-wire`'s `FindingKind::scope`.
///
///   **Nothing structural holds this.** The findings table keys by path and
///   carries no foreign key to `documents` — deliberately, because a
///   place-scoped finding outlives the absence it reports — so what enforces
///   the pairing today is a withholding branch on the record path and a
///   re-derivation condition on the heal. A store at rest is where the two meet,
///   which is why the claim is asked here.
///
///   [ADR 0023]: https://github.com/dbtlr/norn/blob/main/docs/decisions/0023-a-walk-refusal-that-stands-is-a-reading.md
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

    assert_every_finding_stands_where_its_scope_allows(store, subject);

    for_each_stored_suffix_key(store, |stored| {
        assert_eq!(
            stored.raw,
            stored.path.suffix_key(),
            "{subject}: the row at `{}` holds a suffix key its own path does not produce",
            stored.path.as_str()
        );
        assert_eq!(
            stored.folded,
            stored.path.folded_suffix_key(),
            "{subject}: the row at `{}` holds a folded suffix key its own path does not produce",
            stored.path.as_str()
        );
    })
    .unwrap_or_else(|problem| panic!("{subject}: draining the stored suffix keys: {problem}"));
}

/// **The finding-vs-live-row invariant.** Every finding the store holds stands
/// at a path whose document row its kind's scope allows.
///
/// Both directions are read, because each catches a different loss. A
/// place-scoped finding co-resident with a row is a report about a place nothing
/// derived, standing at a place something derived — the withholding that keeps a
/// readable document's spelling from being reported as a collision, lost. A
/// document-scoped finding with no row is a report about a document's
/// frontmatter with no document: whatever pruned the row left the finding, and a
/// reader asking "what is wrong with this document" is answered about one that
/// is not there.
///
/// The findings are drained through the store's own enumerator so an empty
/// answer is an empty pillar, and the row beside each is asked for by key.
fn assert_every_finding_stands_where_its_scope_allows(store: &mut Store, subject: &str) {
    let request = store.begin_request();
    let mut after: Option<FindingCursor> = None;
    loop {
        let page = request
            .stored_findings_after(after, PAGE)
            .unwrap_or_else(|problem| panic!("{subject}: draining the findings pillar: {problem}"));
        let Some((last, _)) = page.last() else {
            return;
        };
        after = Some(*last);
        for (_, finding) in &page {
            let kind = FindingKind::try_from(finding.kind.as_str()).unwrap_or_else(|_| {
                panic!(
                    "{subject}: the finding at `{}` carries the kind `{}`, which is outside the \
                     closed vocabulary",
                    finding.path.as_str(),
                    finding.kind
                )
            });
            let row = request
                .stored_document(&finding.path)
                .unwrap_or_else(|problem| {
                    panic!(
                        "{subject}: reading the row at `{}`: {problem}",
                        finding.path.as_str()
                    )
                });
            match (kind.scope(), row.is_some()) {
                (FindingScope::Place, true) => panic!(
                    "{subject}: the `{kind}` finding at `{}` is place-scoped and a document row \
                     stands there. A place-scoped finding reports that nothing was derivable at \
                     the place, and is withheld while a document occupies it.",
                    finding.path.as_str()
                ),
                (FindingScope::Document, false) => panic!(
                    "{subject}: the `{kind}` finding at `{}` is document-scoped and no document \
                     row stands there. A document-scoped finding is about the document derived at \
                     its subject, so it has nothing to be about.",
                    finding.path.as_str()
                ),
                _ => {}
            }
        }
    }
}

/// Hand every row's stored suffix keys over beside its path, a bounded page at
/// a time.
///
/// The pair comes off the store's own suffix-key enumerator rather than off a
/// document page, so the recompute reaches every row without putting the key
/// column on the readers that page documents for their facts.
fn for_each_stored_suffix_key(
    store: &mut Store,
    mut visit: impl FnMut(&StoredSuffixKeys),
) -> Result<(), StoreError> {
    let request = store.begin_request();
    let mut after: Option<DocumentPath> = None;
    loop {
        let page = request.suffix_keys_after(after.as_ref(), PAGE)?;
        let Some(last) = page.last() else {
            return Ok(());
        };
        after = Some(last.path.clone());
        for stored in &page {
            visit(stored);
        }
    }
}

/// Every tombstone the store holds, drained a bounded page at a time.
///
/// Public because deaths are outside the projection and a suite that changed a
/// tree is what knows which of them are owed: it reads the pillar through here
/// and looks for the places it took documents away from. The drain is the
/// store's own enumerator, so an empty answer is an empty pillar rather than a
/// read that asked about nothing.
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
fn push_indexed<T: StoredColumns>(
    entries: &mut Vec<(String, String)>,
    at: &str,
    name: &str,
    rows: &[T],
) {
    entries.push((format!("{at}.{name} count"), rows.len().to_string()));
    for (ordinal, row) in rows.iter().enumerate() {
        push_columns(entries, &format!("{at}.{name}[{ordinal}]"), row);
    }
}

/// Render one stored row at `at`, one field to a column.
fn push_columns(entries: &mut Vec<(String, String)>, at: &str, row: &impl StoredColumns) {
    for (column, value) in row.columns() {
        entries.push((format!("{at}.{column}"), value));
    }
}

/// What a column holding SQL `NULL` renders as. Every present text value is
/// quoted and every present integer is bare digits, so nothing present
/// renders as this.
const NULL: &str = "(none)";

/// One stored row, as the names of its columns and the values the database
/// holds in them.
///
/// **Every column the row's table carries a derived value in is here**,
/// checked against every derived table's DDL. What is left out, and why:
///
/// - **The row identifier** — `id` on every table that has one, and the
///   `(document, key, ordinal)` primary key `document_fields` uses instead.
///   Where a row landed, never a fact about the vault.
/// - **The owning row's foreign key** — `document` on `links`, `headings`,
///   `blocks`, `document_tags` and `document_fields`; `finding` on
///   `finding_classes` and `finding_candidates`. [`StoreProjection::entries`]
///   already names the row this one stands under in its `at`.
/// - **`ordinal`** on `links`, `headings`, `blocks` and `document_tags`, and
///   **`rank`** on `finding_candidates`. These are read in that order and
///   rendered at their position within their owning row ([`push_indexed`] and
///   the candidates' own enumeration), so the value is carried by where a row
///   stands rather than repeated as a named column. `document_fields` renders
///   its `ordinal` from the stored column instead, because its rows interleave
///   more than one key under one document, where position alone would not say
///   which.
/// - **`generation`** on `findings` and on `documents`, and on the vault-schema
///   pin — write generations, dropped for [`ProjectedFinding`]'s own reason.
/// - **`derived_at`** on `documents` and every other timestamp — when a row was
///   written, never a fact about the vault.
///
/// Every other column is rendered, `document_fields.path` included: the
/// document's own path, copied onto every field row so a field sort can page
/// by it without a join, and [`FieldRow`] renders it so a copy that drifted
/// from the document it names is caught here rather than nowhere.
trait StoredColumns {
    fn columns(&self) -> Vec<(&'static str, String)>;
}

impl StoredColumns for LinkFact {
    fn columns(&self) -> Vec<(&'static str, String)> {
        let mut columns = vec![
            ("family", quoted(self.family.as_str())),
            ("embed", flag(self.embed)),
            ("protocol", optional_text(self.protocol.as_deref())),
            ("target", quoted(&self.target)),
            ("title", optional_text(self.title.as_deref())),
            ("anchor", optional_text(self.anchor.as_deref())),
            ("block_ref", optional_text(self.block_ref.as_deref())),
        ];
        columns.extend(span_columns(Some(self.span)));
        columns
    }
}

impl StoredColumns for HeadingFact {
    fn columns(&self) -> Vec<(&'static str, String)> {
        let mut columns = vec![
            ("text", quoted(&self.text)),
            ("slug", quoted(&self.slug)),
            ("level", self.level.to_string()),
        ];
        columns.extend(span_columns(Some(self.span)));
        columns.push(("body_offset", self.body_offset.to_string()));
        columns.push(("inside_container", flag(self.inside_container)));
        columns
    }
}

impl StoredColumns for BlockFact {
    fn columns(&self) -> Vec<(&'static str, String)> {
        let mut columns = vec![("block_id", quoted(&self.block_id))];
        columns.extend(span_columns(self.span));
        columns
    }
}

impl StoredColumns for TagFact {
    fn columns(&self) -> Vec<(&'static str, String)> {
        let mut columns = vec![
            ("name", quoted(&self.name)),
            ("source", quoted(self.source.as_str())),
        ];
        columns.extend(span_columns(self.span));
        columns
    }
}

/// A presence row stores its container and no value; a value row stores its
/// value and no container. Both store both least-value flags, a presence row's
/// as zero. Both carry `path`, the document's own path copied onto the row: a
/// copy that drifted from the document it names would otherwise be caught
/// nowhere, since no reader joins it back to `documents` to check.
impl StoredColumns for FieldRow {
    fn columns(&self) -> Vec<(&'static str, String)> {
        let (container, raw, typed, least_raw, least_typed) = match self {
            FieldRow::Presence { container, .. } => {
                (Some(container.as_str()), None, None, false, false)
            }
            FieldRow::Value {
                raw,
                typed,
                least_raw,
                least_typed,
                ..
            } => (
                None,
                raw.as_deref(),
                typed.as_deref(),
                *least_raw,
                *least_typed,
            ),
        };
        vec![
            ("key", quoted(self.key())),
            ("ordinal", self.ordinal().to_string()),
            ("path", quoted(self.path())),
            ("container", optional_text(container)),
            ("raw", optional_text(raw)),
            ("typed", optional_text(typed)),
            ("least_raw", flag(least_raw)),
            ("least_typed", flag(least_typed)),
        ]
    }
}

/// A finding's own columns. Its candidates and class memberships are rows of
/// their own tables, rendered beside it by [`StoreProjection::entries`].
impl StoredColumns for ProjectedFinding {
    fn columns(&self) -> Vec<(&'static str, String)> {
        let mut columns = vec![
            ("kind", quoted(&self.kind)),
            ("severity", quoted(&self.severity)),
            ("target", optional_text(self.target.as_deref())),
        ];
        columns.extend(span_columns(self.span.map(
            |(line, column, byte_offset)| Span {
                line,
                column,
                byte_offset,
            },
        )));
        columns.extend([
            ("candidates_total", self.candidates_total.to_string()),
            ("message", quoted(&self.message)),
            ("detail", optional_text(self.detail.as_deref())),
            (
                "vault_schema_fingerprint",
                quoted(&self.vault_schema_fingerprint),
            ),
        ]);
        columns
    }
}

/// The three columns a span is stored in, each [`NULL`] where there is no
/// span.
fn span_columns(span: Option<Span>) -> [(&'static str, String); 3] {
    let column = |value: Option<u64>| value.map_or_else(|| NULL.to_string(), |v| v.to_string());
    [
        ("span_line", column(span.map(|span| span.line))),
        ("span_column", column(span.map(|span| span.column))),
        ("span_offset", column(span.map(|span| span.byte_offset))),
    ]
}

/// A flag as the integer column holding it.
fn flag(value: bool) -> String {
    u8::from(value).to_string()
}

/// A text column that may hold `NULL`.
fn optional_text(value: Option<&str>) -> String {
    value.map_or_else(|| NULL.to_string(), quoted)
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
                body_hash: "body-hash-1".to_string(),
                frontmatter_projection_hash: None,
                byte_length: 7,
                body_offset: 0,
                frontmatter: None,
                frontmatter_diagnostic_count: 0,
                body: "a body\n".to_string(),
                links: Vec::new(),
                headings: Vec::new(),
                blocks: Vec::new(),
                tags: Vec::new(),
                fields: FieldRows::default(),
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

    /// A row renders as the columns its table stores and the values the
    /// columns hold, so what names a field is the column and not the Rust
    /// field or variant carrying it.
    #[test]
    fn a_row_renders_by_its_column_names_and_stored_values() {
        let mut linked = projection();
        linked.documents[0].links.push(LinkFact {
            family: norn_store::LinkFamily::Wikilink,
            embed: true,
            protocol: None,
            target: "Notes".to_string(),
            title: None,
            anchor: None,
            block_ref: Some("para".to_string()),
            span: Span {
                line: 1,
                column: 2,
                byte_offset: 1,
            },
        });
        let entries = linked.entries();
        let at = |column: &str| {
            entries
                .get(&format!("document[docs/a.md].link[0].{column}"))
                .unwrap_or_else(|| panic!("no `{column}` column in {entries:#?}"))
                .as_str()
        };
        assert_eq!(at("family"), "\"wikilink\"");
        assert_eq!(at("embed"), "1");
        assert_eq!(at("protocol"), NULL);
        assert_eq!(at("block_ref"), "\"para\"");
        assert_eq!(at("span_offset"), "1");

        let mut moved = linked.clone();
        moved.documents[0].links[0].block_ref = Some("other".to_string());
        let divergence = linked
            .compare(&moved)
            .divergence
            .expect("a changed stored value diverges");
        assert_eq!(divergence.field, "document[docs/a.md].link[0].block_ref");
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
