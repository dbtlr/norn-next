//! What each write verb is asked for.
//!
//! **A write verb is a plan of its operations.** `set`, `edit` and `new`
//! write one document's frontmatter or text, or create one; `move`,
//! `delete` and `rewrite_wikilink` move, remove or retarget documents and
//! carry the link cascade that follows. Each carries what a person or an
//! agent names in one request — a document, a folder or a predicate list,
//! the changes, the conditions it observed, whether it forces — and each
//! compiles to an [`AuthoredPlan`](crate::AuthoredPlan) by a pure method, so
//! the host plans and applies it through the one planner and the one applier
//! `apply` goes through, and answers with the same
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

use crate::target::ResolutionTarget;

pub(crate) mod delete;
pub(crate) mod edit;
pub(crate) mod init;
pub(crate) mod moves;
pub(crate) mod new;
pub(crate) mod repair;
pub(crate) mod rewrite_wikilink;
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

/// A resolution target read as the whole document it names
/// ([`ResolutionTarget::whole_document`]).
fn document_target<'de, D>(deserializer: D) -> Result<ResolutionTarget, D::Error>
where
    D: Deserializer<'de>,
{
    ResolutionTarget::deserialize(deserializer)?
        .whole_document()
        .map_err(D::Error::custom)
}
