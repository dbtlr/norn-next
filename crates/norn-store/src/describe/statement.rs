//! The statement the describe builder emits, named, and the one composer that
//! spells it with its parameters.
//!
//! **A declared facet runs no statement.** The declared fields, tags, tag
//! patterns, folders, path rules and the undeclared-tag stance are read off
//! the declaration the snapshot pins, held in memory; only the observed field
//! keys are read from the store, and [`DescribeStatement`] names the one
//! statement that reads them.

use norn_db::rusqlite::types::Value;

use crate::fields::FieldContainer;
use crate::read::{Binder, key_walk};

/// Every statement shape the describe builder runs, named.
///
/// The same discipline as [`crate::FindStatement`]: [`DescribeStatement::all`]
/// holds each shape once, [`DescribeStatement::slot`] is exhaustive over the
/// enum, and [`DESCRIBE_STATEMENTS`] is the count a census is checked against.
/// A describe reads the active fingerprint through the statement the find
/// builder names for it, and names it there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DescribeStatement {
    /// A page of the keys documents carry, each with the containers some
    /// document holds it in: the key walk every enumeration of keys shares,
    /// from the page's position and bounded by the page, seeking
    /// `document_fields_presence` once per key it reaches, and for each key one
    /// covering seek of `(key, container)` per container. It reads the index
    /// alone, never a field row, and costs the keys it pages rather than the
    /// documents that carry them.
    ObservedFields,
}

/// How many statement shapes [`DescribeStatement::all`] holds.
pub const DESCRIBE_STATEMENTS: usize = 1;

impl DescribeStatement {
    /// Every statement shape, in slot order.
    pub fn all() -> [Self; DESCRIBE_STATEMENTS] {
        [Self::ObservedFields]
    }

    /// Where this statement stands in [`Self::all`]. Exhaustive, so a shape
    /// added to the enum has to take a slot.
    pub fn slot(self) -> usize {
        let slot = match self {
            Self::ObservedFields => 0,
        };
        assert!(
            slot < DESCRIBE_STATEMENTS,
            "slot {slot} is outside the enumeration: grow `all` and `DESCRIBE_STATEMENTS` with \
             the statement that took it"
        );
        slot
    }
}

/// [`DescribeStatement::ObservedFields`]: at most `rows` keys documents carry,
/// in key order, each after `after` — from the first key where it is `None` —
/// and then one column per container in [`FieldContainer::ALL`]'s order,
/// saying whether any document holds the key in it.
///
/// The walk yields at most `rows` keys, so a page past its bound seeks no key
/// beyond it; each container column is one existence seek at `(key,
/// container)`, so a key held in one container by every document costs the
/// same as a key held once.
pub(crate) fn compose_observed(after: Option<&str>, rows: usize) -> (String, Vec<Value>) {
    let mut binder = Binder::default();
    let (comparison, bound) = match after {
        None => (">=", String::new()),
        Some(key) => (">", key.to_string()),
    };
    let bound = binder.bind(Value::Text(bound));
    let rows = binder.bind(Value::Integer(
        i64::try_from(rows).expect("a page's row count fits i64"),
    ));
    let walk = key_walk(&format!("{comparison} {bound}"), Some(&rows));
    let containers: Vec<String> = FieldContainer::ALL
        .iter()
        .map(|container| {
            let container = binder.bind(Value::Text(container.as_str().to_string()));
            format!(
                "EXISTS (SELECT 1 FROM document_fields AS c
                          WHERE c.ordinal = 0 AND c.key = walked.key AND c.container = {container})"
            )
        })
        .collect();
    (
        format!(
            "{walk}
         SELECT walked.key, {}
           FROM walked WHERE walked.key IS NOT NULL",
            containers.join(",\n                ")
        ),
        binder.into_values(),
    )
}
