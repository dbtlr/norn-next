//! What an embedder refuses with.

use std::error::Error;
use std::fmt;

use crate::model::Model;

/// An embedding that was asked for and not produced.
///
/// Every refusal names the model that refused, because the model is what a
/// caller decides about: retry it, stop asking it, or migrate off it. The
/// decision is read from the variant and its fields — **never from the
/// message**, which is written for a person reading a log.
///
/// The enum and its variants are `#[non_exhaustive]`, so a refusal a runtime
/// brings with it extends this vocabulary rather than breaking every caller
/// that matched on it. Variants are built through their constructors.
///
/// # What is deliberately not here
///
/// Acquiring a model — fetching weights, verifying their digest, loading them
/// — happens once, at the explicit act that enables semantic search, and it
/// refuses there. Those refusals belong to the acquisition surface, and none
/// of them can reach this type: by the time an [`Embedder`](crate::Embedder)
/// exists, its model is loaded.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum EmbedError {
    /// The model was asked for a vector and did not produce one.
    #[non_exhaustive]
    Runtime {
        /// The model that refused.
        model: Model,
        /// What went wrong, for a person. Not a caller's input.
        message: String,
    },
}

impl EmbedError {
    /// `model` was asked for a vector and did not produce one.
    pub fn runtime(model: Model, message: impl Into<String>) -> Self {
        EmbedError::Runtime {
            model,
            message: message.into(),
        }
    }

    /// The model the refusal came from.
    pub fn model(&self) -> &Model {
        match self {
            EmbedError::Runtime { model, .. } => model,
        }
    }
}

impl fmt::Display for EmbedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EmbedError::Runtime { model, message } => {
                write!(f, "model {model} produced no vector: {message}")
            }
        }
    }
}

impl Error for EmbedError {}
