//! A request's conjunction compiled once, for every read builder: the filters
//! its parts spell, and the parts it could not apply as asked.

use std::collections::BTreeSet;

use norn_db::rusqlite::types::Value;
use norn_wire::{Pattern, Predicate, Unsatisfied};

use super::filter::{Filter, ReadFilter};
use super::run::StatementFailure;
use super::{FieldOrder, Lookups, PageRefusal, Ran, ReadBound, glob, suggest};
use crate::error::{self, StoreError};
use crate::fields::DeclaredFields;
use crate::find::{
    FindStatement, compose_bare_directory, compose_known_key, compose_match_probe, compose_universe,
};
use crate::json::{FrontmatterValue, canonical_json};
use crate::path::{DirectoryPrefix, DocumentPath, suffix_probe};
use crate::store::Snapshot;

/// Where a request named a key.
#[derive(Clone, Copy, Debug)]
pub(crate) enum KeyPlace {
    Sort,
    Projection,
    Predicate,
    Group,
}

/// One part of a request that could not be applied as asked, before the
/// field universe an unknown key's suggestions are drawn from is read.
pub(crate) enum Report {
    /// A part reported as it stands.
    Part(Unsatisfied),
    /// A key outside the field universe, named at `KeyPlace`.
    Unknown(KeyPlace, String),
}

/// A conjunction compiled: the filters its parts spell, the parts it could
/// not apply as asked, and whether one of those matches nothing.
pub(crate) struct Conjunction {
    pub(crate) filters: Vec<Filter>,
    pub(crate) reports: Vec<Report>,
    /// Whether some part matches no document, which empties the answer.
    pub(crate) matches_nothing: bool,
}

/// Whether the verb compiling a conjunction answers a `resolves` part.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Resolution {
    /// The part filters by the class its target opens.
    Answered,
    /// The part is reported as not applicable and filters nothing.
    NotApplicable,
}

/// A compiled part of the conjunction.
enum Part {
    Filter(Filter),
    /// A part that cannot be applied as asked, so no document satisfies it:
    /// reported, and every section is empty.
    MatchesNothing(Unsatisfied),
}

impl Snapshot {
    /// Whether `key` is in the field universe: declared, or carried by some
    /// document. A declared key asks the snapshot nothing; any other is one
    /// existence seek, asked once per request.
    pub(crate) fn is_known(
        &self,
        key: &str,
        declared: &DeclaredFields,
        lookups: &mut Lookups,
    ) -> Result<bool, StoreError> {
        if declared.is_declared(key) {
            return Ok(true);
        }
        if let Some(known) = lookups.known.get(key) {
            return Ok(*known);
        }
        let known = self.ask(
            &mut lookups.ran,
            Ran::new(FindStatement::KnownKey, compose_known_key(key)),
            "asking whether a document carries a key",
        )?;
        lookups.known.insert(key.to_string(), known);
        Ok(known)
    }

    /// The field universe: every declared key, and every key a document
    /// carries.
    fn field_universe(
        &self,
        declared: &DeclaredFields,
        lookups: &mut Lookups,
    ) -> Result<BTreeSet<String>, StoreError> {
        const OPERATION: &str = "reading the keys the vault's documents carry";
        let carried: Vec<String> = self
            .run_statement(
                &mut lookups.ran,
                Ran::new(FindStatement::FieldUniverse, compose_universe()),
                |row| row.get(0),
            )
            .map_err(|problem| error::sql(OPERATION, problem))?;
        Ok(carried
            .into_iter()
            .chain(declared.keys().map(str::to_string))
            .collect())
    }

    /// The reports a request's compile made, with an unknown key's
    /// suggestions drawn from the field universe — which is read once, and
    /// only where some key is unknown.
    pub(crate) fn resolve(
        &self,
        reports: Vec<Report>,
        declared: &DeclaredFields,
        lookups: &mut Lookups,
    ) -> Result<Vec<Unsatisfied>, StoreError> {
        let universe = if reports
            .iter()
            .any(|report| matches!(report, Report::Unknown(..)))
        {
            self.field_universe(declared, lookups)?
        } else {
            BTreeSet::new()
        };
        Ok(reports
            .into_iter()
            .map(|report| match report {
                Report::Part(part) => part,
                Report::Unknown(place, key) => {
                    let near = suggest::did_you_mean(&key, universe.iter().map(String::as_str));
                    match place {
                        KeyPlace::Sort => Unsatisfied::unknown_sort_key(key, near),
                        KeyPlace::Projection => Unsatisfied::unknown_projection_key(key, near),
                        KeyPlace::Predicate => Unsatisfied::unknown_predicate_key(key, near),
                        KeyPlace::Group => Unsatisfied::unknown_group_key(key, near),
                    }
                }
            })
            .collect())
    }

    /// A request's conjunction compiled under `declared`: the filters its
    /// parts spell, and the parts it could not apply as asked, in the order
    /// the request names them.
    ///
    /// Every read that filters by a conjunction compiles it here, so a part
    /// means one thing on every verb. A part whose key is outside the field
    /// universe is reported and filters nothing; a part that cannot be applied
    /// is reported and matches nothing. A `resolves` part is compiled into a
    /// filter where `resolution` answers it, and reported as not applicable,
    /// filtering nothing, where it does not.
    pub(crate) fn compile_conjunction(
        &self,
        predicates: &[Predicate],
        resolution: Resolution,
        declared: &DeclaredFields,
        lookups: &mut Lookups,
    ) -> Result<Conjunction, PageRefusal> {
        let mut conjunction = Conjunction {
            filters: Vec::new(),
            reports: Vec::new(),
            matches_nothing: false,
        };
        for predicate in predicates {
            membership_bound(predicate)?;
            if let Predicate::Resolves { target, .. } = predicate
                && resolution == Resolution::NotApplicable
            {
                conjunction
                    .reports
                    .push(Report::Part(Unsatisfied::resolves_not_applicable(
                        target.clone(),
                    )));
                continue;
            }
            if let Some(key) = predicate_key(predicate)
                && !self.is_known(key, declared, lookups)?
            {
                conjunction
                    .reports
                    .push(Report::Unknown(KeyPlace::Predicate, key.to_string()));
                continue;
            }
            match self.compile_predicate(predicate, declared, lookups)? {
                Part::Filter(filter) => conjunction.filters.push(filter),
                Part::MatchesNothing(part) => {
                    conjunction.matches_nothing = true;
                    conjunction.reports.push(Report::Part(part));
                }
            }
        }
        Ok(conjunction)
    }

    /// One part of the conjunction as the filter a statement spells, or the
    /// report that it matches nothing.
    ///
    /// Only a finding part reads the fingerprint, only a path part with no
    /// wildcard probes whether it names a bare directory, and only a match
    /// part probes whether the full-text engine parses its query: the other
    /// parts bind nothing the snapshot has to be asked for.
    fn compile_predicate(
        &self,
        predicate: &Predicate,
        declared: &DeclaredFields,
        lookups: &mut Lookups,
    ) -> Result<Part, PageRefusal> {
        let text = |value: &str| Value::Text(value.to_string());
        let filter =
            |shape: ReadFilter, values: Vec<Value>| Ok(Part::Filter(Filter { shape, values }));
        // The order a key's values compare under, and a request's value as a
        // place in it: its typed sort key where the key carries a typed order,
        // its text where it does not.
        let order = |key: &str| match declared.typed_order(key) {
            None => FieldOrder::Raw,
            Some(_) => FieldOrder::Typed,
        };
        let compared = |key: &String, value: &String| match declared.typed_order(key) {
            None => Ok(value.clone()),
            Some(typed) => typed
                .sort_key(value)
                .ok_or_else(|| PageRefusal::UnreadableBound {
                    key: key.clone(),
                    value: value.clone(),
                }),
        };
        match predicate {
            Predicate::Eq { key, value, .. } => filter(
                ReadFilter::Equal(order(key)),
                vec![text(key), Value::Text(compared(key, value)?)],
            ),
            Predicate::NotEq { key, value, .. } => filter(
                ReadFilter::NotEqual(order(key)),
                vec![text(key), Value::Text(compared(key, value)?)],
            ),
            Predicate::In { key, values, .. } => {
                let listed = canonical_json(&FrontmatterValue::Sequence(
                    values
                        .iter()
                        .map(|value| compared(key, value).map(FrontmatterValue::String))
                        .collect::<Result<Vec<FrontmatterValue>, PageRefusal>>()?,
                ))?;
                filter(
                    ReadFilter::Member(order(key)),
                    vec![text(key), Value::Text(listed)],
                )
            }
            Predicate::Has { key, .. } => filter(ReadFilter::Present, vec![text(key)]),
            Predicate::Missing { key, .. } => filter(ReadFilter::Absent, vec![text(key)]),
            Predicate::Before { key, value, .. } | Predicate::After { key, value, .. } => {
                let (order, bound) = (order(key), compared(key, value)?);
                let shape = if matches!(predicate, Predicate::Before { .. }) {
                    ReadFilter::Before(order)
                } else {
                    ReadFilter::After(order)
                };
                filter(shape, vec![text(key), Value::Text(bound)])
            }
            Predicate::Matches { query, .. } => match self.match_problem(query, lookups)? {
                Some(problem) => Ok(Part::MatchesNothing(Unsatisfied::malformed_query(
                    query.clone(),
                    problem,
                ))),
                None => filter(ReadFilter::FullText, vec![text(query)]),
            },
            Predicate::Path { glob, .. } => match Pattern::parse(glob) {
                Err(problem) => Ok(Part::MatchesNothing(Unsatisfied::malformed_glob(
                    glob.clone(),
                    problem.to_string(),
                ))),
                Ok(pattern) => {
                    if let Some(part) = self.unmatchable_path(&pattern, lookups)? {
                        return Ok(Part::MatchesNothing(part));
                    }
                    let (lower, upper) = glob::path_range(&pattern);
                    filter(
                        ReadFilter::PathGlob,
                        vec![Value::Text(lower), upper, text(glob)],
                    )
                }
            },
            Predicate::LinksTo { .. } => Err(PageRefusal::NotIndexed {
                fact: "a link's target",
            }),
            Predicate::Resolves { target, .. } => match suffix_probe(target.address()) {
                Err(_) => Ok(Part::MatchesNothing(Unsatisfied::impossible_path(
                    target.address(),
                ))),
                Ok(probe) => filter(
                    ReadFilter::Resolves,
                    probe
                        .ranges()
                        .flat_map(|(lower, upper)| [text(lower), text(upper)])
                        .collect(),
                ),
            },
            Predicate::Tag { name, .. } => filter(ReadFilter::Tag, vec![text(name)]),
            Predicate::HasFinding { kind, .. } => filter(
                ReadFilter::Finding,
                // A finding recorded under no schema is stamped with the
                // empty fingerprint.
                vec![
                    Value::Text(self.fingerprint(lookups)?.unwrap_or_default()),
                    text(kind.as_str()),
                ],
            ),
            _ => Err(PageRefusal::UnknownPart {
                part: "a predicate",
            }),
        }
    }

    /// What the full-text engine says is wrong with `query`, or `None` where it
    /// parses. One [`FindStatement::MatchProbe`].
    ///
    /// A probe that does not prepare or bind is the store's problem and
    /// refused as one, whatever code it failed with: the query is bound as a
    /// value, so nothing before the probe is stepped reads it. Only a failure
    /// met stepping the prepared probe can be the request's, and it is read as
    /// that exactly where [`query_problem`] says so.
    fn match_problem(
        &self,
        query: &str,
        lookups: &mut Lookups,
    ) -> Result<Option<String>, StoreError> {
        const OPERATION: &str = "asking whether a full-text query parses";
        let answered = self.run_statement_staged(
            &mut lookups.ran,
            Ran::new(FindStatement::MatchProbe, compose_match_probe(query)),
            |row| row.get::<_, bool>(0),
        );
        match answered {
            Ok(_) => Ok(None),
            Err(StatementFailure::Stepping(problem)) => match query_problem(&problem) {
                Some(said) => Ok(Some(said)),
                None => Err(error::sql(OPERATION, problem)),
            },
            Err(StatementFailure::Preparing(problem)) => Err(error::sql(OPERATION, problem)),
        }
    }

    /// The report a parsed glob is answered with where it can match nothing by
    /// construction, or `None` where it can be applied.
    ///
    /// A glob with no wildcard is asked first whether it is a bare directory —
    /// no document at the path, and some beneath it — because that is the
    /// report that says what the request meant. Then any glob is impossible
    /// where, read with each wildcard as a letter, it is no document path the
    /// store accepts: every path the glob could match is spelled that way with
    /// other characters in the holes, and the refusals the grammar makes are
    /// about the separators and segments a hole does not change.
    fn unmatchable_path(
        &self,
        pattern: &Pattern,
        lookups: &mut Lookups,
    ) -> Result<Option<Unsatisfied>, StoreError> {
        let source = pattern.as_str();
        let literal = !source.contains(['*', '?']);
        if literal && let Ok(directory) = DirectoryPrefix::new(source) {
            let (lower, upper) = directory.descendant_bounds();
            let bare = self.ask(
                &mut lookups.ran,
                Ran::new(
                    FindStatement::BareDirectory,
                    compose_bare_directory(source, &lower, &upper),
                ),
                "asking whether a path names a bare directory",
            )?;
            if bare {
                return Ok(Some(Unsatisfied::bare_directory(source)));
            }
        }
        if DocumentPath::new(&source.replace(['*', '?'], "a")).is_err() {
            return Ok(Some(Unsatisfied::impossible_path(source)));
        }
        Ok(None)
    }
}

/// What `problem`, met stepping a prepared [`FindStatement::MatchProbe`], says
/// is wrong with the query it bound, or `None` where it is a problem of the
/// store's.
///
/// The full-text engine parses a query when the probe is stepped, and reports
/// every query it cannot read — `fts5: syntax error near …`, an unterminated
/// string, a column filter naming no column — as the plain `SQLITE_ERROR`, with
/// its words as the message. A damaged index, a failed read, a busy or an
/// interrupted connection each report a code of their own, and stay the
/// store's.
fn query_problem(problem: &norn_db::rusqlite::Error) -> Option<String> {
    match problem {
        norn_db::rusqlite::Error::SqliteFailure(failure, message)
            if failure.extended_code == norn_db::rusqlite::ffi::SQLITE_ERROR =>
        {
            Some(message.clone().unwrap_or_else(|| failure.to_string()))
        }
        _ => None,
    }
}

/// The key a predicate names, where it names one.
fn predicate_key(predicate: &Predicate) -> Option<&str> {
    match predicate {
        Predicate::Eq { key, .. }
        | Predicate::NotEq { key, .. }
        | Predicate::In { key, .. }
        | Predicate::Has { key, .. }
        | Predicate::Missing { key, .. }
        | Predicate::Before { key, .. }
        | Predicate::After { key, .. } => Some(key),
        _ => None,
    }
}

/// A membership part's values held to `1..=`[`super::IN_VALUES_CEILING`]; every other
/// part passes.
fn membership_bound(predicate: &Predicate) -> Result<(), PageRefusal> {
    let Predicate::In { key, values, .. } = predicate else {
        return Ok(());
    };
    if values.is_empty() {
        return Err(PageRefusal::EmptyMembership { key: key.clone() });
    }
    if values.len() > ReadBound::MembershipValues.ceiling() {
        return Err(PageRefusal::OutOfBound {
            bound: ReadBound::MembershipValues,
            given: values.len(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use norn_db::rusqlite::{Error, ffi};

    use super::query_problem;

    fn failure(extended_code: i32, message: &str) -> Error {
        Error::SqliteFailure(ffi::Error::new(extended_code), Some(message.to_string()))
    }

    /// The engine's words for a query it cannot read are the request's
    /// problem; a damaged index, a failed read, a busy or an interrupted
    /// connection is the store's, whatever its message says.
    #[test]
    fn only_a_query_the_engine_cannot_read_is_the_requests_problem() {
        for said in [
            "fts5: syntax error near \"\"",
            "unterminated string",
            "no such column: title",
        ] {
            assert_eq!(
                query_problem(&failure(ffi::SQLITE_ERROR, said)).as_deref(),
                Some(said)
            );
        }
        for code in [
            ffi::SQLITE_CORRUPT_VTAB,
            ffi::SQLITE_CORRUPT,
            ffi::SQLITE_IOERR_READ,
            ffi::SQLITE_BUSY,
            ffi::SQLITE_INTERRUPT,
            ffi::SQLITE_NOMEM,
        ] {
            assert_eq!(
                query_problem(&failure(code, "fts5: syntax error near \"\"")),
                None,
                "code {code} was read as the request's"
            );
        }
        assert_eq!(query_problem(&Error::QueryReturnedNoRows), None);
    }
}
