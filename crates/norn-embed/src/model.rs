//! Which model produced a vector.

use std::fmt;

/// A model's identity: what it is, and which revision of it.
///
/// **Both halves are part of the identity, and neither is decoration.** A
/// vector is a pure function of `(model id, model version, content)`, so a
/// version that moves is a different function and the vectors it produced are
/// no longer comparable with the ones before it. Storing the pair beside every
/// vector is what makes a model upgrade a migration over `(model_id,
/// model_version)` rather than a silent reinterpretation of stored bytes.
///
/// The version is a string rather than a number because what moves it is a
/// release-time decision, not an arithmetic one.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Model {
    id: String,
    version: String,
}

impl Model {
    /// A model identified by `id` at `version`.
    pub fn new(id: impl Into<String>, version: impl Into<String>) -> Self {
        Model {
            id: id.into(),
            version: version.into(),
        }
    }

    /// Which model.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Which revision of it.
    pub fn version(&self) -> &str {
        &self.version
    }
}

impl fmt::Display for Model {
    /// `id/version` — the spelling a log line carries. It is a rendering, not
    /// a parseable identity: an id holding a slash renders ambiguously, and
    /// nothing reads a model back out of this.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.id, self.version)
    }
}
