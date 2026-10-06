//! A rule set's one spelling at rest, and the one way its names are read back.
//!
//! **A set is held once, as its canonical spelling.** `rule_sets.rules` is the
//! names of the set's rules in byte order, each once, as the canonical JSON of
//! a list of them. It is both what a finding's write finds the set by — the
//! unique `(fingerprint, rules)` index — and what every reader reads the names
//! back from, through SQLite's `json_each`, so the set's members are not held
//! a second time to drift from the key. `finding_rules` is the one other place
//! a finding's rules stand, and the store's verification holds it equal to the
//! set the finding cites.
//!
//! **A spelling is read back only if it is the spelling of what it holds.**
//! [`members`] takes the elements `json_each` walked out of a spelling and
//! refuses any that are not text, an empty set, names out of byte order or
//! named twice, and a spelling that is not the canonical spelling of its names
//! — so a key two sets could share under two spellings is damage rather than a
//! second set.

use std::collections::BTreeSet;

use crate::error::StoreError;
use crate::json::{FrontmatterValue, canonical_json};

/// The canonical spelling of the set holding `rules`: their names in byte
/// order, each once, as the canonical JSON of a list of them.
pub(crate) fn spelling(rules: &BTreeSet<String>) -> Result<String, StoreError> {
    canonical_json(&FrontmatterValue::Sequence(
        rules
            .iter()
            .map(|rule| FrontmatterValue::String(rule.clone()))
            .collect(),
    ))
}

/// The names of the set `id` spelled `spelled`, from the elements `json_each`
/// walked out of it in order — each the element's text, or `None` for an
/// element that is not text — or a description of why the row is no set this
/// crate wrote.
pub(crate) fn members(
    id: i64,
    spelled: &str,
    elements: Vec<Option<String>>,
) -> Result<Vec<String>, StoreError> {
    let damaged = |problem: &str| StoreError::Damaged {
        what: format!("the rule set {id}, spelled `{spelled}`, {problem}"),
    };
    let names: Vec<String> = elements
        .into_iter()
        .collect::<Option<Vec<String>>>()
        .ok_or_else(|| damaged("names a rule by something other than text"))?;
    if names.is_empty() {
        return Err(damaged("holds no rule"));
    }
    if !names.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(damaged("names its rules out of byte order or twice"));
    }
    let held: BTreeSet<String> = names.iter().cloned().collect();
    if spelling(&held)? != spelled {
        return Err(damaged(
            "is not the canonical spelling of the rules it holds",
        ));
    }
    Ok(names)
}

/// The columns a statement reading a set's names selects, in the order
/// [`walked`] reads them, for the set aliased `set` whose spelling `json_each`
/// walks under the alias `element`: the set's identity, its spelling, the
/// element's place — `NULL` where the walk met no element, as a `LEFT JOIN`
/// answers an empty set — and the element's text, `NULL` for an element that
/// is not text.
pub(crate) fn walked_columns(set: &str, element: &str) -> String {
    format!(
        "{set}.id, {set}.rules, {element}.key, \
         CASE {element}.type WHEN 'text' THEN {element}.value END"
    )
}

/// One row of a statement walking sets' names: whose row it is — a finding's
/// or the set's own identity — the set, its spelling, and the element walked,
/// `None` where the walk met none and `Some(None)` for one that is not text.
pub(crate) struct Walked {
    owner: i64,
    set: i64,
    spelled: String,
    element: Option<Option<String>>,
}

impl Walked {
    /// Whose row this is: a finding's identity, or the set's own.
    pub(crate) const fn owner(&self) -> i64 {
        self.owner
    }
}

/// The row of a statement selecting the owner at `owner` and
/// [`walked_columns`] from `first` on.
pub(crate) fn walked(
    row: &norn_db::rusqlite::Row<'_>,
    owner: usize,
    first: usize,
) -> norn_db::rusqlite::Result<Walked> {
    let key: Option<norn_db::rusqlite::types::Value> = row.get(first + 2)?;
    Ok(Walked {
        owner: row.get(owner)?,
        set: row.get(first)?,
        spelled: row.get(first + 1)?,
        element: key.map(|_| row.get(first + 3)).transpose()?,
    })
}

/// The names of each set the rows walk, by owner, in the order the rows
/// stand: each owner's rows consecutive, as a statement ordered by the owner
/// hands them back. A set that is no set this crate wrote is damage
/// ([`members`]).
pub(crate) fn owned_sets(rows: Vec<Walked>) -> Result<Vec<(i64, i64, Vec<String>)>, StoreError> {
    let mut sets: Vec<(i64, i64, String, Vec<Option<String>>)> = Vec::new();
    for row in rows {
        match sets.last_mut() {
            Some((owner, _, _, elements)) if *owner == row.owner => {
                elements.extend(row.element);
            }
            _ => sets.push((
                row.owner,
                row.set,
                row.spelled,
                row.element.into_iter().collect(),
            )),
        }
    }
    sets.into_iter()
        .map(|(owner, set, spelled, elements)| Ok((owner, set, members(set, &spelled, elements)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    fn elements(names: &[&str]) -> Vec<Option<String>> {
        names.iter().map(|name| Some((*name).to_string())).collect()
    }

    #[test]
    fn a_sets_spelling_reads_back_as_its_names() {
        let spelled = spelling(&set(&["tasks", "open-tasks"])).expect("a spelling");
        assert_eq!(spelled, r#"["open-tasks","tasks"]"#);
        assert_eq!(
            members(1, &spelled, elements(&["open-tasks", "tasks"])).expect("a set"),
            vec!["open-tasks".to_string(), "tasks".to_string()]
        );
    }

    /// Every way a row can fail to be a set this crate wrote is damage: an
    /// element that is not text, no element, names out of order or twice, and
    /// a spelling that is not the canonical one of its names.
    #[test]
    fn a_spelling_that_is_no_sets_is_damage() {
        for (spelled, read) in [
            (r#"["a",1]"#, vec![Some("a".to_string()), None]),
            ("[]", Vec::new()),
            (r#"["b","a"]"#, elements(&["b", "a"])),
            (r#"["a","a"]"#, elements(&["a", "a"])),
            (r#"[ "a" ]"#, elements(&["a"])),
            (r#""a""#, elements(&["a"])),
        ] {
            assert!(
                matches!(members(7, spelled, read), Err(StoreError::Damaged { .. })),
                "`{spelled}` read back as a set"
            );
        }
    }
}
