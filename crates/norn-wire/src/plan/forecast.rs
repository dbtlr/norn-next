//! What a plan would do, as a preview and a refusal report it.
//!
//! **A forecast is an answer, not a plan.** It is read the way every answer
//! is, dropping a field it does not know.
//!
//! **A forecast carries only what the plan beside it does not.** It always
//! crosses beside a resolved plan, which already names each target and its
//! two states, so a forecast repeating them would carry every transition of
//! a vault-wide preview twice. What the plan cannot say is left: which targets
//! drifted, the folders the plan makes and removes, the schema violations its
//! force lets through, the links a caller should look at, and the files a
//! folder move leaves behind.
//!
//! **A drifted target is marked, not judged.** A fresh plan resolved after a
//! refusal marks every target that drifted, because a hash cannot tell a file
//! edited after this plan landed on it from one edited before: the mark says
//! the target may already carry this plan's change, and applying the fresh
//! plan is the caller's decision. A preview resolved from what the vault
//! holds has no drifted target.
//!
//! **A force is loud.** A forced plan bypasses the schema check and nothing
//! else, and the forecast lists every schema violation the force lets
//! through, in the shape a refusal would carry it, so a caller reads what it
//! is forcing before it applies. The applied report lists them again.
//!
//! **A folder is its own path type, and so is a file that is not a
//! document.** Folders are not transitions, and the forecast and the applied
//! report name the folders a plan makes and removes. A document path's grammar
//! would fit a folder, but its published description says it is where a
//! document stands, so a folder path is the same two rules under a name and a
//! description of its own. A folder move takes only the documents its folder
//! holds, and the forecast names every other file it leaves there — an
//! attachment, a file the vault does not read — by a file path: the same two
//! rules again, since neither a document path nor a folder path describes it.
//!
//! **A link advisory points into the change set rather than repeating it.**
//! A resolved plan records every link whose resolution it changes, before and
//! after, as a condition. What that record cannot say is left to the
//! forecast: a link the plan's cascade matched and left as written, and why,
//! and a link whose new resolution a caller should look at — left broken,
//! made ambiguous, or an ambiguous link the plan retargets. Each advisory
//! names its link by the key the change set holds it under, never by a
//! resolution of its own.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::address::IllegalPath;
use crate::document::DocumentPath;
use crate::plan::document::LinkKey;
use crate::plan::outcome::SchemaViolation;

/// A path relative to the vault root under a name and a description of its
/// own: the two rules a document path keeps — it names something, and it
/// does not start at a filesystem root — read through on every door.
macro_rules! vault_relative_path {
    (
        $(#[$doc:meta])*
        $name:ident, $what:literal, $schema_description:literal
    ) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// The path `text` spells, or the reason it spells none.
            pub fn new(text: impl AsRef<str>) -> Result<Self, IllegalPath> {
                let text = text.as_ref();
                if text.is_empty() {
                    return Err(IllegalPath::new(
                        text,
                        $what,
                        concat!("a ", $what, " names something rather than nothing"),
                    ));
                }
                if text.starts_with('/') {
                    return Err(IllegalPath::new(
                        text,
                        $what,
                        concat!("a ", $what, " is relative to the vault root"),
                    ));
                }
                Ok($name(text.to_string()))
            }

            /// The path as the string it is.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            /// A path arrives as the string it is written as and is read
            /// through the grammar, so a path that crossed is a path that
            /// parsed.
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let text = String::deserialize(deserializer)?;
                $name::new(text).map_err(D::Error::custom)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                Cow::Borrowed(stringify!($name))
            }

            fn schema_id() -> Cow<'static, str> {
                Cow::Borrowed(concat!("norn_wire::", stringify!($name)))
            }

            /// A string with a floor of one character, and the rule against a
            /// leading slash stated in the description, as a document path
            /// states it.
            fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "string",
                    "description": $schema_description,
                    "minLength": 1,
                })
            }
        }
    };
}

vault_relative_path!(
    /// Where a folder stands in its vault, relative to the vault root.
    ///
    /// On the wire a folder path is the string itself: `"notes/archive"`. It is
    /// not empty and it does not start with a slash.
    FolderPath,
    "folder path",
    "Where a folder stands in its vault, relative to the vault root. Not empty, and never starting with a slash."
);

vault_relative_path!(
    /// Where a file that is not a vault document stands in its vault, relative
    /// to the vault root: an attachment, or any other file the vault does not
    /// read as a document.
    ///
    /// On the wire a file path is the string itself: `"notes/diagram.png"`. It
    /// is not empty and it does not start with a slash.
    FilePath,
    "file path",
    "Where a file that is not a vault document stands in its vault, relative to the vault root. Not empty, and never starting with a slash."
);

/// What a plan does to one link that its resolution change set does not say.
///
/// On the wire an advisory is an object tagged `advisory`, naming its link by
/// the key the change set holds it under:
/// `{"advisory":"left_broken","link":{"holder":"notes/c.md","syntax":"wikilink","address":"b"}}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "advisory", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LinkAdvisory {
    /// The link resolves to several documents, so whether it names the one
    /// the plan moves or removes is not known, and it is left as written.
    #[non_exhaustive]
    SkippedAmbiguous {
        /// The link.
        link: LinkKey,
    },
    /// The new address is not one the link's syntax can spell where its
    /// address is written, or would read back as something else there, so
    /// the link is left as written.
    #[non_exhaustive]
    SkippedUnrepresentable {
        /// The link.
        link: LinkKey,
    },
    /// The link is written in a frontmatter value that cannot hold the new
    /// address and still read as the same value with only its address
    /// changed, so it is left as written.
    #[non_exhaustive]
    SkippedWouldCorruptFrontmatter {
        /// The link.
        link: LinkKey,
    },
    /// The link's own bytes give no place to write any address — a wikilink
    /// spanning a line break, a Markdown destination written with escapes —
    /// so it is left as written.
    #[non_exhaustive]
    SkippedNotRewritable {
        /// The link.
        link: LinkKey,
    },
    /// The plan leaves the link broken, as a delete allowing broken links
    /// says it may.
    #[non_exhaustive]
    LeftBroken {
        /// The link.
        link: LinkKey,
    },
    /// The plan makes the link ambiguous: it resolves to several documents
    /// after the plan where it did not before.
    #[non_exhaustive]
    MadeAmbiguous {
        /// The link.
        link: LinkKey,
    },
    /// The link was ambiguous and the plan changes what it resolves to — a
    /// delete leaving one of its candidates, say — without rewriting it.
    #[non_exhaustive]
    Retargeted {
        /// The link.
        link: LinkKey,
    },
}

impl LinkAdvisory {
    /// `link` is ambiguous, and left as written.
    pub const fn skipped_ambiguous(link: LinkKey) -> Self {
        LinkAdvisory::SkippedAmbiguous { link }
    }

    /// `link` cannot spell its new address, and is left as written.
    pub const fn skipped_unrepresentable(link: LinkKey) -> Self {
        LinkAdvisory::SkippedUnrepresentable { link }
    }

    /// `link`'s frontmatter value cannot hold its new address, and it is left
    /// as written.
    pub const fn skipped_would_corrupt_frontmatter(link: LinkKey) -> Self {
        LinkAdvisory::SkippedWouldCorruptFrontmatter { link }
    }

    /// `link`'s bytes give no place to write an address, and it is left as
    /// written.
    pub const fn skipped_not_rewritable(link: LinkKey) -> Self {
        LinkAdvisory::SkippedNotRewritable { link }
    }

    /// The plan leaves `link` broken.
    pub const fn left_broken(link: LinkKey) -> Self {
        LinkAdvisory::LeftBroken { link }
    }

    /// The plan makes `link` ambiguous.
    pub const fn made_ambiguous(link: LinkKey) -> Self {
        LinkAdvisory::MadeAmbiguous { link }
    }

    /// The plan changes what the ambiguous `link` resolves to.
    pub const fn retargeted(link: LinkKey) -> Self {
        LinkAdvisory::Retargeted { link }
    }
}

/// What a resolved plan would do that the plan itself does not say: which of
/// its targets drifted, the folders it makes and removes, what it forces, the
/// links a caller should look at, and the files a folder move leaves behind.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Forecast {
    /// Each target that held neither its before-state nor its after-state
    /// when the plan was resolved afresh. A drifted target may already carry
    /// this plan's change. Empty on a preview.
    pub drifted: Vec<DocumentPath>,
    /// The folders the plan makes for the files it creates.
    pub folders_made: Vec<FolderPath>,
    /// The folders the plan's removals leave empty, which it removes.
    pub folders_removed: Vec<FolderPath>,
    /// Every schema violation a result carries that the plan's force lets
    /// through. Empty for a plan that is not forced, and for a forced plan
    /// whose every result is valid.
    pub forced: Vec<SchemaViolation>,
    /// What the plan does to each link a caller should look at that its
    /// resolution change set does not say: a link its cascade left as
    /// written and why, a link it leaves broken or makes ambiguous, and an
    /// ambiguous link it retargets.
    pub links: Vec<LinkAdvisory>,
    /// Every file that is not a document, left in a folder the plan's folder
    /// moves move from: a folder move takes only the documents it holds.
    pub left_behind: Vec<FilePath>,
}

impl Forecast {
    /// A plan whose targets `drifted` drifted, making `folders_made` and
    /// removing `folders_removed`, forcing nothing through, advising on no
    /// link and leaving no file behind.
    pub const fn new(
        drifted: Vec<DocumentPath>,
        folders_made: Vec<FolderPath>,
        folders_removed: Vec<FolderPath>,
    ) -> Self {
        Forecast {
            drifted,
            folders_made,
            folders_removed,
            forced: Vec::new(),
            links: Vec::new(),
            left_behind: Vec::new(),
        }
    }

    /// The forecast of a plan advising `links` about the links it touches.
    #[must_use]
    pub fn with_links(mut self, links: Vec<LinkAdvisory>) -> Self {
        self.links = links;
        self
    }

    /// The forecast of a plan whose folder moves leave `left_behind`.
    #[must_use]
    pub fn with_left_behind(mut self, left_behind: Vec<FilePath>) -> Self {
        self.left_behind = left_behind;
        self
    }

    /// The forecast of a forced plan letting `forced` through.
    #[must_use]
    pub fn with_forced(mut self, forced: Vec<SchemaViolation>) -> Self {
        self.forced = forced;
        self
    }
}
