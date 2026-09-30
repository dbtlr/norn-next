//! What each document-local write verb is asked for.
//!
//! **A write verb is a plan of its operations.** `set`, `edit` and `new` each
//! carry what a person or an agent names in one request — a document or a
//! predicate list, the changes, the conditions it observed, whether it forces
//! — and each compiles to an [`AuthoredPlan`](crate::AuthoredPlan) by a pure
//! method, so the host plans and applies it through the one planner and the
//! one applier `apply` goes through, and answers with the same
//! [`ApplyReport`](crate::ApplyReport). No write verb has a report or a code
//! of its own.
//!
//! **A write states its mode, as `apply` does.** There is no default: a
//! request naming neither `preview` nor `apply` does not read.
//!
//! **A write request refuses a field it does not know, as a plan does.** Each
//! request becomes a plan, and a field dropped on the way in — a condition a
//! newer caller wrote — would weaken the check the plan was to carry. Its
//! changes and edits refuse one too, and a request carrying no change or no
//! edit is refused rather than read as a plan that does nothing.
//!
//! **A request's conditions are carried by every operation it compiles to.**
//! A condition is what the author observed and requires to hold before any of
//! the request's changes, so each operation carries all of them, and an
//! operation whose condition no longer holds is left unresolved with the
//! others on its document.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer};

pub(crate) mod edit;
pub(crate) mod new;
pub(crate) mod set;

/// A list read with the floor of one member, refused as `what` when empty.
fn at_least_one<'de, D, T>(deserializer: D, what: &str) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let members = Vec::<T>::deserialize(deserializer)?;
    if members.is_empty() {
        return Err(D::Error::custom(format_args!(
            "a write request names at least one {what}"
        )));
    }
    Ok(members)
}

/// Whether a flag is left out of a request's bytes: `false`, which is what
/// its absence reads as. serde hands the field by reference.
const fn is_false(flag: &bool) -> bool {
    !*flag
}
