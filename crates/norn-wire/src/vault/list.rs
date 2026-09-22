//! `vault list`: every registration this installation holds.
//!
//! **A verb with nothing to ask for still has a params type.** `ListParams`
//! carries no field, and it exists so every verb in the registry is spelled by
//! one params type and one report type: a surface renders a request for a
//! listing the way it renders every other request, and a field this verb gains
//! later arrives on a type its callers already name.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::status::Registration;

/// What a `vault list` request carries: nothing.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ListParams {}

impl ListParams {
    /// A request for every registration.
    pub const fn new() -> Self {
        ListParams {}
    }
}

/// What `vault list` answers with: every registration, in name order.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ListReport {
    /// The registrations, ascending by name. A registry holding none is an
    /// empty list rather than a refusal.
    pub registrations: Vec<Registration>,
}

impl ListReport {
    /// The listing of `registrations`.
    pub fn new(registrations: impl IntoIterator<Item = Registration>) -> Self {
        ListReport {
            registrations: registrations.into_iter().collect(),
        }
    }
}
