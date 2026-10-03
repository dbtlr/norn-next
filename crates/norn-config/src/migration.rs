//! The migration ladders: how each vault control file is brought up to the
//! version this build reads.
//!
//! **A ladder is a version reader and one step per version.** A file states
//! the version it is written at ([`Ladder::version_of`]); a step reads the
//! text of a file at one version and writes the text of the next
//! ([`Step`]). [`Ladder::migrate`] walks a file from the version it states to
//! [`Ladder::current`], one step per version, and answers the rewritten text,
//! or that the file is current and nothing is rewritten.
//!
//! **A step is a text transform, not a re-rendering.** It edits only what its
//! version change touches and leaves every other byte of the file as the
//! author wrote it — key order, spacing and comments included — so no
//! normalizer runs and no comment-aware YAML or TOML editor is needed. What
//! holds a step to that is the comment rule: a rewrite whose output does not
//! hold every comment its input holds is refused, naming the first comment
//! lost ([`MigrationRefusal::CommentLost`]), rather than the comment lost. A
//! comment is the text from a `#` that begins one, in the file's format
//! ([`Format`]), to the end of its line, its trailing whitespace aside; one
//! moved elsewhere in the file is kept.
//!
//! **What refuses, and why each is a refusal.** A file whose version cannot
//! be read — not UTF-8, not in its format, or stating no version — is
//! refused as unreadable, naming why. A version ahead of [`Ladder::current`]
//! was written by a later build, and reading it under this one's meanings
//! would drop what this build does not know. A version behind with no step
//! from it is one this build has no rewrite for, and a guess at what it
//! meant would be a guess at a format the ladder never held.
//!
//! **The shipped ladders are empty.** The schema has had one grammar version,
//! [`SCHEMA_VERSION`], and the config one shape, so neither holds a step: a
//! file either is current or is refused. The machinery is whole so that the
//! first grammar change is one step added to its ladder. **The ladders are a
//! dormant carrier** in that sense: the layer that consumes a step is the
//! first change to either grammar, and no call graph reaches a step until a
//! version is added. That first step carries one more obligation the empty
//! ladder does not: a vault whose schema is behind attaches only where
//! [`crate::schema::VaultSchema::parse`] reads the older version, since
//! `vault migrate` plans over an attached vault.
//!
//! Pure, as the rest of this crate is: a ladder reads the bytes a caller
//! hands it and touches no file.

mod comments;

use norn_wire::{ControlFile, MigrationRefusal};

use crate::schema::{SCHEMA_VERSION, stated_version};
use crate::vault::VaultConfig;

/// The format a control file is written in, which says what begins a
/// comment in it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    /// YAML: a `#` at the start of a line or after whitespace, outside a
    /// quoted or block scalar.
    Yaml,
    /// TOML: a `#` outside a string.
    Toml,
}

/// One rung of a ladder: the text of a file at version `from` rewritten to
/// the text of the same file at `from + 1`.
#[derive(Clone, Copy, Debug)]
pub struct Step {
    /// The version the step reads.
    pub from: i64,
    /// The rewrite: the whole text in, the whole text out, editing only what
    /// the version change touches.
    pub rewrite: fn(&str) -> String,
}

/// How one control file is brought up to the version this build reads.
#[derive(Clone, Debug)]
pub struct Ladder {
    /// The format the file is written in.
    pub format: Format,
    /// The version this build reads.
    pub current: i64,
    /// The version a file's text states, or why it states none, in words.
    pub version_of: fn(&str) -> Result<i64, String>,
    /// The steps, each from its own version. A version behind `current`
    /// with no step from it is refused.
    pub steps: Vec<Step>,
}

/// The version every vault config is at: the config grammar states none and
/// has had one shape, so its ladder has the one rung every config stands on.
const CONFIG_VERSION: i64 = 1;

impl Ladder {
    /// The vault schema's ladder: YAML, at [`SCHEMA_VERSION`], its version the
    /// integer `version` the schema states. It holds no step.
    pub fn schema() -> Ladder {
        Ladder {
            format: Format::Yaml,
            current: SCHEMA_VERSION,
            version_of: |text| stated_version(text).map_err(|error| error.to_string()),
            steps: Vec::new(),
        }
    }

    /// The vault config's ladder: TOML, and every config that reads as one
    /// is current. It holds no step.
    pub fn config() -> Ladder {
        Ladder {
            format: Format::Toml,
            current: CONFIG_VERSION,
            version_of: |text| {
                VaultConfig::parse(Some(text.as_bytes()))
                    .map(|_| CONFIG_VERSION)
                    .map_err(|error| error.to_string())
            },
            steps: Vec::new(),
        }
    }

    /// The ladder of the control file `file`.
    pub fn of(file: ControlFile) -> Ladder {
        match file {
            ControlFile::Schema => Ladder::schema(),
            ControlFile::Config => Ladder::config(),
        }
    }

    /// The text `bytes` migrate to: `None` where the file is at
    /// [`Self::current`] already, the rewritten text where it is behind, or
    /// why it is not migrated.
    pub fn migrate(&self, bytes: &[u8]) -> Result<Option<String>, MigrationRefusal> {
        let text = std::str::from_utf8(bytes).map_err(|error| {
            MigrationRefusal::unreadable(format!("the file is not UTF-8: {error}"))
        })?;
        let stated = (self.version_of)(text).map_err(MigrationRefusal::unreadable)?;
        if stated > self.current {
            return Err(MigrationRefusal::version_ahead(stated, self.current));
        }
        if stated == self.current {
            return Ok(None);
        }
        let mut rewritten = text.to_string();
        let mut found = stated;
        while found < self.current {
            // A ladder missing any step between the file and the current
            // version has no rewrite for the file, whichever rung is missing.
            let Some(step) = self.steps.iter().find(|step| step.from == found) else {
                return Err(MigrationRefusal::no_step(stated, self.current));
            };
            rewritten = (step.rewrite)(&rewritten);
            found += 1;
        }
        if let Some(comment) = comments::first_lost(self.format, text, &rewritten) {
            return Err(MigrationRefusal::comment_lost(comment));
        }
        Ok(Some(rewritten))
    }
}
