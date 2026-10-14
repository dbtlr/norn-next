//! Repair-time field tallies are streamed through searches of one key at a time.
//!
//! Presence rows supply containers and maps; value rows supply parsed scalar
//! types. These reads derive no stored advisory and hold no document payload.

use norn_db::EmittedPlan;
use norn_db::rusqlite::types::Value;
use norn_wire::{AdvisoryValueType, ContainerKind, DocumentPathHead, ShapeUse, TypeUse};

use crate::error::{self, StoreError};
use crate::fields::FieldContainer;
use crate::read::{Ran, ReadStatement, wire_path};
use crate::store::Snapshot;
use crate::validate::ValidateWork;

/// The statement families repair advisory reads run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepairAdvisoryStatement {
    /// The first distinct key, including an empty key.
    FirstKey,
    /// The next distinct key strictly after a key.
    NextKey,
    /// One key's value rows, ordered by parsed type and document.
    ValueTypes,
    /// One key's presence rows, merged in path order.
    FieldPresence,
    /// The union of spellings' documents, merged in path order.
    SpellingPresence,
}

/// The number of statement families repair advisory reads name.
pub const REPAIR_ADVISORY_STATEMENTS: usize = 5;

impl RepairAdvisoryStatement {
    /// Every statement, in its stable slot order.
    pub const fn all() -> [Self; REPAIR_ADVISORY_STATEMENTS] {
        [
            Self::FirstKey,
            Self::NextKey,
            Self::ValueTypes,
            Self::FieldPresence,
            Self::SpellingPresence,
        ]
    }

    /// The statement's stable slot in a census.
    pub const fn slot(self) -> usize {
        match self {
            Self::FirstKey => 0,
            Self::NextKey => 1,
            Self::ValueTypes => 2,
            Self::FieldPresence => 3,
            Self::SpellingPresence => 4,
        }
    }
}

impl From<RepairAdvisoryStatement> for ReadStatement {
    fn from(statement: RepairAdvisoryStatement) -> Self {
        Self::RepairAdvisory(statement)
    }
}

/// One advisory statement with the plan of the text and values it ran.
#[derive(Clone, Debug)]
pub struct RepairAdvisoryPlan {
    /// The statement family.
    pub statement: RepairAdvisoryStatement,
    /// SQLite's plan of the statement that ran.
    pub plan: EmittedPlan,
}

/// The next distinct field key and the work that read it.
#[derive(Clone, Debug)]
pub struct RepairKeyRead {
    /// The key, or nothing once every key has been read.
    pub key: Option<String>,
    /// The work the seek ran.
    pub work: ValidateWork,
}

/// One key's observed types, containers and distinct document count.
#[derive(Clone, Debug)]
pub struct RepairFieldTally {
    /// Each observed type, in its column's byte order.
    pub types: Vec<TypeUse>,
    /// Each observed container, in `FieldContainer::ALL` order.
    pub shapes: Vec<ShapeUse>,
    /// The distinct documents carrying the key.
    pub documents: u64,
    /// The scalar value rows the key carries.
    pub value_rows: u64,
    /// The first five paths carrying the key, with its exact document total.
    pub sample: DocumentPathHead,
    /// The work the tally ran.
    pub work: ValidateWork,
}

/// The distinct documents holding any spelling and their bounded sample.
#[derive(Clone, Debug)]
pub struct RepairSpellingTally {
    /// The exact size of the union of documents holding the spellings.
    pub documents: u64,
    /// The first five paths holding any spelling, with the exact union total.
    pub sample: DocumentPathHead,
    /// The work the spelling tally ran.
    pub work: ValidateWork,
}

impl Snapshot {
    /// The first key, or the first key strictly after `after`, by one index search.
    pub fn repair_key_after(&self, after: Option<&str>) -> Result<RepairKeyRead, StoreError> {
        self.run_repair_key(after, &mut Vec::new())
    }

    /// The plan of the distinct-key search that runs for `after`.
    pub fn repair_key_after_plans(
        &self,
        after: Option<&str>,
    ) -> Result<Vec<RepairAdvisoryPlan>, StoreError> {
        let mut ran = Vec::new();
        self.run_repair_key(after, &mut ran)?;
        self.repair_advisory_plans(ran)
    }

    fn run_repair_key(
        &self,
        after: Option<&str>,
        ran: &mut Vec<Ran>,
    ) -> Result<RepairKeyRead, StoreError> {
        let (statement, comparison) = if after.is_some() {
            (RepairAdvisoryStatement::NextKey, ">")
        } else {
            (RepairAdvisoryStatement::FirstKey, ">=")
        };
        let sql = format!(
            "SELECT key FROM document_fields INDEXED BY document_fields_presence WHERE ordinal = 0 AND key {comparison} ?1 ORDER BY key LIMIT 1"
        );
        let rows = self
            .run_statement(
                ran,
                Ran::new(
                    statement,
                    (sql, vec![Value::Text(after.unwrap_or("").to_string())]),
                ),
                |row| row.get::<_, String>(0),
            )
            .map_err(|problem| error::sql("reading a repair advisory key", problem))?;
        let work = work_of(ran, rows.len() as u64);
        Ok(RepairKeyRead {
            key: rows.into_iter().next(),
            work,
        })
    }

    /// One key's types and containers, tallying each document once per type.
    pub fn repair_field_tally(&self, key: &str) -> Result<RepairFieldTally, StoreError> {
        self.run_repair_field_tally(key, &mut Vec::new())
    }

    /// The plans of the value-type and presence searches the tally runs.
    pub fn repair_field_tally_plans(
        &self,
        key: &str,
    ) -> Result<Vec<RepairAdvisoryPlan>, StoreError> {
        let mut ran = Vec::new();
        self.run_repair_field_tally(key, &mut ran)?;
        self.repair_advisory_plans(ran)
    }

    fn run_repair_field_tally(
        &self,
        key: &str,
        ran: &mut Vec<Ran>,
    ) -> Result<RepairFieldTally, StoreError> {
        let mut types: Vec<(String, u64)> = Vec::new();
        let mut previous: Option<(String, i64)> = None;
        let mut value_rows = 0;
        self.run_statement(ran, Ran::new(RepairAdvisoryStatement::ValueTypes, (
            "SELECT value_type, document FROM document_fields INDEXED BY document_fields_value_type WHERE key = ?1 AND value_type IS NOT NULL ORDER BY value_type, document".to_string(),
            vec![Value::Text(key.to_string())],
        )), |row| {
            let value_type: String = row.get(0)?;
            let document: i64 = row.get(1)?;
            value_rows += 1;
            if previous.as_ref().is_none_or(|(kind, id)| kind != &value_type || *id != document) {
                if let Some((_, count)) = types.last_mut().filter(|(last_type, _)| last_type == &value_type) {
                    *count += 1;
                } else {
                    types.push((value_type.clone(), 1));
                }
            }
            previous = Some((value_type, document));
            Ok(())
        }).map_err(|problem| error::sql("tallying repair advisory value types", problem))?;
        let mut counts = [0_u64; 3];
        let mut documents = 0;
        let mut sample_paths = Vec::new();
        let (sql, values) = presence_merge(&[key.to_string()]);
        self.run_statement(
            ran,
            Ran::new(RepairAdvisoryStatement::FieldPresence, (sql, values)),
            |row| {
                let container: String = row.get(1)?;
                let at = FieldContainer::ALL
                    .iter()
                    .position(|kind| kind.as_str() == container)
                    .expect("the presence column carries a container");
                counts[at] += 1;
                if documents < crate::CANDIDATE_HEAD as u64 {
                    let path: String = row.get(0)?;
                    sample_paths.push(path);
                }
                documents += 1;
                Ok(())
            },
        )
        .map_err(|problem| error::sql("tallying repair advisory containers", problem))?;
        if counts[2] > 0 {
            let at = types.partition_point(|(kind, _)| kind.as_str() < "map");
            types.insert(at, ("map".to_string(), counts[2]));
        }
        let types = types
            .into_iter()
            .map(|(kind, documents)| TypeUse::new(advisory_type(&kind), documents))
            .collect();
        let shapes = counts
            .into_iter()
            .zip([
                ContainerKind::Scalar,
                ContainerKind::Sequence,
                ContainerKind::Map,
            ])
            .filter(|(documents, _)| *documents > 0)
            .map(|(documents, kind)| ShapeUse::new(kind, documents))
            .collect();
        Ok(RepairFieldTally {
            types,
            shapes,
            documents,
            value_rows,
            sample: document_sample(sample_paths, documents)?,
            work: work_of(ran, value_rows + documents),
        })
    }

    /// The exact union count of documents holding any of `keys`.
    pub fn repair_spelling_tally(
        &self,
        keys: &[String],
    ) -> Result<RepairSpellingTally, StoreError> {
        self.run_repair_spelling_tally(keys, &mut Vec::new())
    }

    /// The plan of the ordered presence searches the spelling tally merges.
    pub fn repair_spelling_tally_plans(
        &self,
        keys: &[String],
    ) -> Result<Vec<RepairAdvisoryPlan>, StoreError> {
        let mut ran = Vec::new();
        self.run_repair_spelling_tally(keys, &mut ran)?;
        self.repair_advisory_plans(ran)
    }

    fn run_repair_spelling_tally(
        &self,
        keys: &[String],
        ran: &mut Vec<Ran>,
    ) -> Result<RepairSpellingTally, StoreError> {
        let mut previous = None;
        let mut rows_read = 0;
        let mut documents = 0;
        let mut sample_paths = Vec::new();
        let composed = presence_merge(keys);
        self.run_statement(
            ran,
            Ran::new(RepairAdvisoryStatement::SpellingPresence, composed),
            |row| {
                let path: String = row.get(0)?;
                rows_read += 1;
                if previous.as_ref() != Some(&path) {
                    if documents < crate::CANDIDATE_HEAD as u64 {
                        sample_paths.push(path.clone());
                    }
                    documents += 1;
                    previous = Some(path);
                }
                Ok(())
            },
        )
        .map_err(|problem| error::sql("tallying repair advisory spellings", problem))?;
        Ok(RepairSpellingTally {
            documents,
            sample: document_sample(sample_paths, documents)?,
            work: work_of(ran, rows_read),
        })
    }

    fn repair_advisory_plans(&self, ran: Vec<Ran>) -> Result<Vec<RepairAdvisoryPlan>, StoreError> {
        self.explained(ran, |statement, _, plan| {
            let ReadStatement::RepairAdvisory(statement) = statement else {
                unreachable!("an advisory read records its own statements")
            };
            RepairAdvisoryPlan { statement, plan }
        })
    }
}

/// Merge each spelling and container's ordered presence seek without sorting rows.
fn presence_merge(keys: &[String]) -> (String, Vec<Value>) {
    let mut values = Vec::new();
    let arms = keys.iter().flat_map(|key| FieldContainer::ALL.into_iter().map(move |container| (key, container)))
        .map(|(key, container)| {
            let key_at = values.len() + 1;
            values.push(Value::Text(key.clone()));
            let container_at = values.len() + 1;
            values.push(Value::Text(container.as_str().to_string()));
            format!("SELECT path, container FROM document_fields INDEXED BY document_fields_presence WHERE ordinal = 0 AND key = ?{key_at} AND container = ?{container_at}")
        }).collect::<Vec<_>>();
    (
        format!(
            "{} ORDER BY path COLLATE NOCASE, path",
            arms.join(" UNION ALL ")
        ),
        values,
    )
}

fn work_of(ran: &[Ran], rows_read: u64) -> ValidateWork {
    let mut work = ValidateWork {
        statements: ran.len() as u64,
        rows_read,
        ..ValidateWork::default()
    };
    for statement in ran {
        work.full_scan_steps += statement.stepped.full_scan_steps;
        work.sorts += statement.stepped.sorts;
        work.vm_steps += statement.stepped.vm_steps;
    }
    work
}

fn advisory_type(kind: &str) -> AdvisoryValueType {
    match kind {
        "string" => AdvisoryValueType::String,
        "integer" => AdvisoryValueType::Integer,
        "real" => AdvisoryValueType::Real,
        "boolean" => AdvisoryValueType::Boolean,
        "null" => AdvisoryValueType::Null,
        "map" => AdvisoryValueType::Map,
        _ => unreachable!("the checked field columns carry a known type"),
    }
}

/// The paths already retained by a tally, bounded to five, with its exact total.
fn document_sample(paths: Vec<String>, total: u64) -> Result<DocumentPathHead, StoreError> {
    let paths = paths
        .into_iter()
        .map(|path| wire_path(&path))
        .collect::<Result<Vec<_>, _>>()?;
    DocumentPathHead::new(paths, total).map_err(|problem| StoreError::Damaged {
        what: problem.to_string(),
    })
}
