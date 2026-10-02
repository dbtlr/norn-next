//! The two plan documents a caller holds: operations it authored, and the
//! resolved plan a preview answers with.
//!
//! **A resolved plan is a self-contained value.** It names its vault by
//! address, carries the identity of the vault's root, its operations, one
//! transition per file they touch and the conditions its planning read. It
//! never carries the bytes of a file it did not author, and it carries no
//! local path to the vault and no format version: a plan is short-lived, and
//! a plan this build cannot read is refused rather than migrated.
//!
//! **Three kinds of fact, three places.** An author's condition sits on an
//! operation; the conditions a resolved plan carries are [`PlanCondition`]s —
//! a file's content, or one entry of the plan's resolution change set —
//! checked when it is applied; and [`Provenance`] records what a plan was
//! planned from and is never checked. Keeping them three types means a
//! condition cannot be mistaken for provenance, and an author's condition
//! that became a before-state is not carried twice.
//!
//! **A plan carries its own force.** Either plan says whether it applies past
//! the schema check, and the applier reads it from the plan, so a forced
//! preview sent back applies the same way. It bypasses the schema check and
//! nothing else — never create exclusivity, drift, a condition or the root's
//! identity. The flag defaults to `false` and is written only when `true`:
//! absent can only mean the strict reading, so a plan that omits it — every
//! plan made before it existed included — is checked in full, and forcing is
//! something a plan says out loud.
//!
//! **A resolved plan names its documents by path.** Planning expands a
//! frontmatter kind's `where` target into one operation per matched document,
//! and a folder move into one document move per document the folder holds, so
//! a resolved plan still carrying either is a fault in its shape, named by
//! [`ResolvedPlan::unexpanded_targets`] and answered `request/plan-invalid`.
//! It is judged rather than refused at the read, so it answers with that code
//! and the operations it names. A `create_by_rule` is expanded the same way,
//! into one `create_document` holding the path and content its rule makes, so
//! a resolved plan still carrying one is named by
//! [`ResolvedPlan::unexpanded_rules`] and answered `request/plan-invalid`.
//!
//! **Planning writes a cascade; an author does not.** A resolved plan's
//! document move, document removal and wikilink rewrite carry the link
//! rewrites their planning generated, so a cascade on an authored operation,
//! or on any other kind, is a fault in the plan's shape — judged, as an
//! unexpanded target is, by [`AuthoredPlan::misplaced_cascades`] and
//! [`ResolvedPlan::misplaced_cascades`].
//!
//! **A control-file plan changes nothing else** (ADR 0032): a plan writing a
//! vault control file beside an operation on documents is a fault in its
//! shape, judged by [`AuthoredPlan::control_files_beside_documents`] and
//! [`ResolvedPlan::control_files_beside_documents`].
//!
//! **Provenance is a dormant carrier for Layer 5 repair.** Repair plans cite
//! the finding generation they read and the findings they skipped. No Layer 4
//! planner emits provenance — every Layer 4 plan is authored by a write verb
//! or a caller, and neither plans from findings — so the current call graph
//! reaches it only through a caller sending it back. It is spelled here so a
//! repair plan is the same document every other plan is.
//!
//! **Each plan carries its own tag, so a resolved plan is its own retry
//! token.** A resolved plan crosses inside every answer an apply gives after
//! planning, and a caller finishes or retries by sending those bytes back
//! unchanged. So the `plan` tag is a field of each plan rather than of the
//! document around it: [`AuthoredPlan`] is always written `"plan":"operations"`
//! and [`ResolvedPlan`] always `"plan":"resolved"`, and each refuses a missing
//! or another tag when read on its own. The tag is a public field whose type
//! is a zero-sized marker, [`OperationsTag`] or [`ResolvedTag`], so the derive
//! writes it, requires it on the read, refuses any other value, and advertises
//! it as a required constant — where serde's struct-level `tag` writes the tag
//! but neither reads nor advertises it — and a plan is still written and
//! destructured as a literal outside this crate.
//!
//! **A document reads its plan by that tag.** [`PlanDocument`] is not a
//! serde-tagged enum, because an internally tagged enum consumes the tag
//! before its variant reads the rest, and each plan must see its own; nor
//! does it hold the object in a buffer until its tag is known, because a
//! buffer keeps the last of two values written for one key where the plan
//! alone refuses the second, and reads a format's own values otherwise than
//! the plan alone does. Its read path is one private shape the derive reads
//! directly — the tag and every field either plan names, no other — and the
//! plan the tag names is then built from the fields it takes, refusing a
//! field only the other plan names. A document is written as the plan it
//! holds, so a plan and the document holding it are one set of bytes.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

use crate::address::VaultAddress;
use crate::document::{DocumentPath, LinkFamily};
use crate::plan::hash::ContentHash;
use crate::plan::operation::{Operation, OperationKind, written};
use crate::plan::outcome::PlanFault;
use crate::plan::root::RootIdentity;
use crate::plan::write_target::WriteTarget;

/// What a file holds on one side of a transition: nothing, or exactly the
/// bytes with a hash, and whether those bytes decode as a vault document.
///
/// On the wire a state is an object tagged `state`: `{"state":"absent"}`,
/// `{"state":"present","hash":"sha256:…"}`, and
/// `{"state":"present","hash":"sha256:…","quarantined":true}` for bytes
/// that do not decode.
///
/// **Whether the bytes decode rides beside their hash**, because a plan's
/// resolution change set reads a file as a link's candidate only where it is
/// a document, and the applier must read that on each side of a transition
/// alike whether or not the bytes are still there to decode: a target that
/// already holds its change has no before-bytes left. The flag is a function
/// of the bytes alone, so every present state in a plan that shares a hash
/// must carry the same flag, whether or not any of those bytes are held.
/// Where the applier holds bytes with a hash — a file still holds them, or it
/// composed them again — every state with that hash must carry the flag they
/// decode to, a gone side included, and a plan saying otherwise is not what
/// its operations do; only a hash no held bytes share is read as recorded. A
/// file that stops decoding is held out of derived state as a quarantined
/// document, which is the name the flag carries.
///
/// The flag is `false` unless it is written `true`, and is left out where it
/// is `false`: absent can only mean the bytes decode, so every plan made
/// before it existed, and every state of a file that decodes, reads and
/// writes as it did. Which bytes a file holds is their hash alone; the flag
/// follows from them.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileState {
    /// Nothing stands at the path.
    Absent {},
    /// The file at the path holds exactly the bytes with this hash.
    Present {
        /// The hash of what the file holds.
        hash: ContentHash,
        /// Whether those bytes do not decode as a vault document, so that
        /// no document stands at the path. Absent is `false`, and `false` is
        /// left out.
        #[serde(default, skip_serializing_if = "is_false")]
        quarantined: bool,
    },
}

impl FileState {
    /// Nothing stands at the path.
    pub const fn absent() -> Self {
        FileState::Absent {}
    }

    /// The file holds the bytes whose hash is `hash`, which decode as a
    /// vault document.
    pub const fn present(hash: ContentHash) -> Self {
        FileState::Present {
            hash,
            quarantined: false,
        }
    }

    /// The file holds the bytes whose hash is `hash`, which do not decode
    /// as a vault document.
    pub const fn quarantined(hash: ContentHash) -> Self {
        FileState::Present {
            hash,
            quarantined: true,
        }
    }

    /// Whether a document stands at the path: a file is there, and its
    /// bytes decode as one.
    pub const fn is_document(&self) -> bool {
        matches!(
            self,
            FileState::Present {
                quarantined: false,
                ..
            }
        )
    }

    /// The hash of the bytes the file holds, where it holds any.
    pub const fn hash(&self) -> Option<&ContentHash> {
        match self {
            FileState::Absent {} => None,
            FileState::Present { hash, .. } => Some(hash),
        }
    }

    /// Whether `self` and `other` hold the same bytes: both absent, or both
    /// present with one hash. Whether the bytes decode follows from them, so
    /// it is not compared.
    pub fn same_content(&self, other: &FileState) -> bool {
        self.hash() == other.hash()
    }
}

/// One file's change within a resolved plan: what it must hold before the
/// write and what it holds after. A file already holding its after-state has
/// landed.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    /// The file changed.
    pub path: DocumentPath,
    /// What the file must hold before the write.
    pub before: FileState,
    /// What the file holds after it.
    pub after: FileState,
}

impl Transition {
    /// The file at `path` changing from `before` to `after`.
    pub const fn new(path: DocumentPath, before: FileState, after: FileState) -> Self {
        Transition {
            path,
            before,
            after,
        }
    }
}

/// One link, as a plan names it: the document holding it, its syntax and its
/// address — what decides which documents it resolves to.
///
/// **A key names a link as it stands at the plan's after-state.** A link a
/// cascade rewrites is keyed by its new address, not the one it is written
/// with before the plan. A link the after-state no longer holds — an old
/// address a rewrite replaced, a link in a removed document — has no key: its
/// disappearance is the plan's own transition, guarded by that file's hashes.
///
/// On the wire a key is one object:
/// `{"holder":"notes/c.md","syntax":"wikilink","address":"vault://notes/a"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinkKey {
    /// The document holding the link.
    pub holder: DocumentPath,
    /// The syntax the link is written in.
    pub syntax: LinkFamily,
    /// The link's address, exactly as written, its protocol prefix included:
    /// `vault://notes/a` for a link written with the `vault` protocol, and
    /// `notes/a` for one written with none. An anchor-only link, `[[#h]]`,
    /// has the empty address, and resolves to its holder wherever that
    /// stands.
    pub address: String,
}

impl LinkKey {
    /// The link of `syntax` in the document at `holder`, written with
    /// `address`.
    pub fn new(holder: DocumentPath, syntax: LinkFamily, address: impl Into<String>) -> Self {
        LinkKey {
            holder,
            syntax,
            address: address.into(),
        }
    }
}

/// What one link resolves to: exactly one document, none, or several.
///
/// On the wire a resolution is an object tagged `resolves`:
/// `{"resolves":"one","path":"notes/a.md"}`, `{"resolves":"none"}`,
/// `{"resolves":"several"}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "resolves", rename_all = "snake_case", deny_unknown_fields)]
pub enum Resolves {
    /// The link resolves to exactly this document.
    One {
        /// The document it resolves to.
        path: DocumentPath,
    },
    /// The link resolves to no document: it is broken.
    None {},
    /// The link resolves to more than one document: it is ambiguous.
    Several {},
}

impl Resolves {
    /// The link resolves to the document at `path`.
    pub const fn one(path: DocumentPath) -> Self {
        Resolves::One { path }
    }

    /// The link resolves to no document.
    pub const fn none() -> Self {
        Resolves::None {}
    }

    /// The link resolves to more than one document.
    pub const fn several() -> Self {
        Resolves::Several {}
    }
}

/// A fact a resolved plan depends on, which must still hold when it is
/// applied.
///
/// **Two kinds of fact.** A content hash is a fact about a file the plan does
/// not write. A link resolution is one entry of the plan's resolution change
/// set: how one link resolves with every target of the plan at its
/// before-state, and with every target at its after-state, so a plan's own
/// progress never changes it. Its link may sit in a file the plan writes — a
/// link a cascade rewrites has an entry — so it is not a fact about a file
/// the plan leaves alone.
///
/// **An entry is keyed at the after-state.** Its key names the link
/// as the plan leaves it: a rewritten link by its new address, its `before`
/// being what that key resolved to from its holder before the plan. A link
/// the after-state does not hold — an old address a rewrite replaced, a link
/// in a removed document — is no entry: its disappearance is the plan's own
/// transition, which that file's hashes guard.
///
/// **The change set is exact.** It holds one entry for every link whose
/// resolution the plan changes, and no other, so the applier computes it
/// again and refuses on any difference: an entry the plan records that the
/// set computed again does not hold, and an entry the set computed again
/// holds that the plan does not record.
///
/// On the wire a condition is an object tagged `condition`:
/// `{"condition":"content_hash","path":"notes/c.md","hash":"sha256:…"}`,
/// `{"condition":"link_resolution","link":{…},"before":{"resolves":"one","path":"a.md"},"after":{"resolves":"none"}}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "condition", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanCondition {
    /// The file at the path holds exactly the bytes with this hash.
    ContentHash {
        /// The file the condition is about.
        path: DocumentPath,
        /// The hash of what it must hold.
        hash: ContentHash,
    },
    /// One link resolves as recorded before the plan, and as recorded after
    /// it: one entry of the plan's resolution change set, keyed by the link
    /// as it stands at the plan's after-state.
    LinkResolution {
        /// The link, as it stands at the plan's after-state.
        link: LinkKey,
        /// What its key resolves to from its holder with every target at its
        /// before-state.
        before: Resolves,
        /// What it resolves to with every target at its after-state.
        after: Resolves,
    },
}

impl PlanCondition {
    /// The file at `path` holds the bytes whose hash is `hash`.
    pub const fn content_hash(path: DocumentPath, hash: ContentHash) -> Self {
        PlanCondition::ContentHash { path, hash }
    }

    /// `link` resolves to `before` before the plan and to `after` after it.
    pub const fn link_resolution(link: LinkKey, before: Resolves, after: Resolves) -> Self {
        PlanCondition::LinkResolution {
            link,
            before,
            after,
        }
    }
}

/// A finding a repair plan left alone, and why.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkippedFinding {
    /// The finding's identity in the vault's findings.
    pub finding: u64,
    /// Why the plan left it alone, in words, for a person reading the plan.
    pub reason: String,
}

impl SkippedFinding {
    /// The finding `finding`, left alone for `reason`.
    pub fn new(finding: u64, reason: impl Into<String>) -> Self {
        SkippedFinding {
            finding,
            reason: reason.into(),
        }
    }
}

// A dormant carrier: Layer 5 repair is the consuming layer. Repair plans cite
// the finding generation they read and the findings they skipped; no Layer 4
// planner plans from findings, so nothing in the current call graph emits
// this, and it is reached only when a caller sends a plan carrying one back.
// Its published description stays wire-facing, so the roadmap note lives here
// rather than in the doc comment schemars lifts.
/// What a repair plan was planned from. It is a record, never checked when
/// the plan is applied.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    /// The write generation of the findings the plan was planned from.
    pub finding_generation: u64,
    /// The findings the plan left alone.
    pub skipped: Vec<SkippedFinding>,
}

impl Provenance {
    /// A plan planned from the findings at `finding_generation`, leaving
    /// `skipped` alone.
    pub const fn new(finding_generation: u64, skipped: Vec<SkippedFinding>) -> Self {
        Provenance {
            finding_generation,
            skipped,
        }
    }
}

/// The constant a plan type writes under `plan` and requires on the read: a
/// public zero-sized marker, so a plan is written and destructured as a
/// literal outside this crate, whose read refuses any other value and whose
/// schema is the constant inline.
macro_rules! plan_tag {
    ($tag:ident, $string:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
        pub struct $tag;

        impl Serialize for $tag {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str($string)
            }
        }

        impl<'de> Deserialize<'de> for $tag {
            /// The tag is read as a one-member vocabulary, so any other value
            /// is refused as a variant nobody minted.
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                #[derive(Deserialize)]
                enum Only {
                    #[serde(rename = $string)]
                    Tag,
                }
                Only::deserialize(deserializer).map(|Only::Tag| $tag)
            }
        }

        impl JsonSchema for $tag {
            fn inline_schema() -> bool {
                true
            }

            fn schema_name() -> Cow<'static, str> {
                Cow::Borrowed(stringify!($tag))
            }

            fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "string",
                    "const": $string,
                })
            }
        }
    };
}

plan_tag!(
    OperationsTag,
    "operations",
    "The tag an authored plan is written under: always `operations`."
);
plan_tag!(
    ResolvedTag,
    "resolved",
    "The tag a resolved plan is written under: always `resolved`."
);

/// A plan as its author writes it: the vault it is for and its operations,
/// not yet resolved against what the vault holds.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoredPlan {
    /// Which plan this is: always `operations`.
    pub plan: OperationsTag,
    /// The vault the plan is for.
    pub vault: VaultAddress,
    /// The operations, in the order they compose.
    pub operations: Vec<Operation>,
    /// Whether the plan applies past the schema check: a result that
    /// violates the vault schema is written, and listed as forced. Nothing
    /// else is bypassed. Absent is `false`, and `false` is left out.
    #[serde(default, skip_serializing_if = "is_false")]
    pub force: bool,
    /// Words about the plan, for a person reading it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footnote: Option<String>,
}

impl AuthoredPlan {
    /// The plan for `vault`, made of `operations`, not forced.
    pub const fn new(vault: VaultAddress, operations: Vec<Operation>) -> Self {
        AuthoredPlan {
            plan: OperationsTag,
            vault,
            operations,
            force: false,
            footnote: None,
        }
    }

    /// The plan applying past the schema check where `force` holds.
    #[must_use]
    pub const fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    /// The plan carrying `footnote`.
    #[must_use]
    pub fn with_footnote(mut self, footnote: impl Into<String>) -> Self {
        self.footnote = Some(footnote.into());
        self
    }

    /// The fault of a plan whose operations planning expands — those with a
    /// `where` target, and folder moves — carry an identifier or a
    /// requirement, naming each such operation by its position; `None` where
    /// none does.
    ///
    /// **An expanded operation is expanded before anything orders it**: into
    /// one operation per document the vault matches, or the folder holds,
    /// before the plan, so an identifier on it would name several operations,
    /// and a requirement would order it after an operation whose result it
    /// never expands over.
    pub fn ordered_expanded_targets(&self) -> Option<PlanFault> {
        let positions: Vec<usize> = self
            .operations
            .iter()
            .enumerate()
            .filter(|(_, operation)| {
                (matches!(operation.kind.target(), Some(WriteTarget::Where(_)))
                    || matches!(operation.kind, OperationKind::MoveFolder { .. }))
                    && (operation.id.is_some() || !operation.requires.is_empty())
            })
            .map(|(position, _)| position)
            .collect();
        (!positions.is_empty()).then(|| PlanFault::expanded_target_ordered(positions))
    }

    /// The fault of a plan whose operations carry a link cascade, naming each
    /// such operation by its position; `None` where none does.
    ///
    /// **A cascade is planning's to write**: it is read off the links the
    /// vault holds when the plan is resolved, so an authored one would either
    /// repeat what planning finds or claim a rewrite no link calls for.
    pub fn misplaced_cascades(&self) -> Option<PlanFault> {
        let positions: Vec<usize> = self
            .operations
            .iter()
            .enumerate()
            .filter(|(_, operation)| !operation.cascade.is_empty())
            .map(|(position, _)| position)
            .collect();
        (!positions.is_empty()).then(|| PlanFault::misplaced_cascade(positions))
    }

    /// The fault of a plan writing a vault control file beside an operation
    /// on documents, naming each control-file write by its position; `None`
    /// where the plan writes only control files or only documents.
    ///
    /// **A plan that changes a vault control file changes nothing else** (ADR
    /// 0032). A control file is what every document is judged under, so a
    /// plan changing both would judge its documents under one declaration
    /// and land them under another; it is split instead, one plan for each.
    pub fn control_files_beside_documents(&self) -> Option<PlanFault> {
        control_files_beside_documents(&self.operations)
    }
}

/// The fault of `operations` writing a vault control file beside an operation
/// on documents, naming each control-file write by its position: the one rule
/// both plans are judged by.
fn control_files_beside_documents(operations: &[Operation]) -> Option<PlanFault> {
    let positions: Vec<usize> = operations
        .iter()
        .enumerate()
        .filter(|(_, operation)| operation.kind.writes_control_file())
        .map(|(position, _)| position)
        .collect();
    let beside = operations.len() > positions.len();
    (!positions.is_empty() && beside).then(|| PlanFault::control_file_beside_documents(positions))
}

/// A plan resolved against what the vault held: its operations, one transition
/// per file they touch, the conditions its planning read, and the identity of
/// the root it was resolved against. Sending it back applies it; sending it
/// again after an interruption finishes it.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedPlan {
    /// Which plan this is: always `resolved`.
    pub plan: ResolvedTag,
    /// The vault the plan is for.
    pub vault: VaultAddress,
    /// The identity of the vault's root when the plan was resolved. A vault
    /// whose root identity differs refuses the plan.
    pub root: RootIdentity,
    /// The operations, in the order they compose.
    pub operations: Vec<Operation>,
    /// One per file the operations touch. Folders are not transitions.
    pub transitions: Vec<Transition>,
    /// What must still hold of the files the plan does not write.
    pub conditions: Vec<PlanCondition>,
    /// What a repair plan was planned from. Absent from every other plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    /// Whether the plan applies past the schema check, as the operations it
    /// was resolved from asked: a result that violates the vault schema is
    /// written, and listed as forced. Nothing else is bypassed. Absent is
    /// `false`, and `false` is left out.
    #[serde(default, skip_serializing_if = "is_false")]
    pub force: bool,
    /// Words about the plan, for a person reading it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footnote: Option<String>,
}

impl ResolvedPlan {
    /// The plan for `vault`, resolved against the root `root` into
    /// `transitions`, depending on `conditions`, not forced, with no
    /// provenance and no footnote.
    pub const fn new(
        vault: VaultAddress,
        root: RootIdentity,
        operations: Vec<Operation>,
        transitions: Vec<Transition>,
        conditions: Vec<PlanCondition>,
    ) -> Self {
        ResolvedPlan {
            plan: ResolvedTag,
            vault,
            root,
            operations,
            transitions,
            conditions,
            provenance: None,
            force: false,
            footnote: None,
        }
    }

    /// The plan citing `provenance`.
    #[must_use]
    pub fn with_provenance(mut self, provenance: Provenance) -> Self {
        self.provenance = Some(provenance);
        self
    }

    /// The plan applying past the schema check where `force` holds.
    #[must_use]
    pub const fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    /// The fault of a resolved plan whose operations still carry a `where`
    /// target or move a folder, naming each such operation by its position;
    /// `None` where every operation names its documents by path, as planning
    /// makes them.
    pub fn unexpanded_targets(&self) -> Option<PlanFault> {
        let positions: Vec<usize> = self
            .operations
            .iter()
            .enumerate()
            .filter(|(_, operation)| {
                matches!(operation.kind.target(), Some(WriteTarget::Where(_)))
                    || matches!(operation.kind, OperationKind::MoveFolder { .. })
            })
            .map(|(position, _)| position)
            .collect();
        (!positions.is_empty()).then(|| PlanFault::unexpanded_target(positions))
    }

    /// The fault of a resolved plan whose operations still create a document
    /// by rule, naming each such operation by its position; `None` where
    /// planning expanded every one into a `create_document`.
    pub fn unexpanded_rules(&self) -> Option<PlanFault> {
        let positions: Vec<usize> = self
            .operations
            .iter()
            .enumerate()
            .filter(|(_, operation)| matches!(operation.kind, OperationKind::CreateByRule { .. }))
            .map(|(position, _)| position)
            .collect();
        (!positions.is_empty()).then(|| PlanFault::unexpanded_rule(positions))
    }

    /// The fault of a resolved plan whose operations carry a link cascade on
    /// a kind that does not cascade, naming each such operation by its
    /// position; `None` where only a document move, a document removal or a
    /// wikilink rewrite carries one.
    pub fn misplaced_cascades(&self) -> Option<PlanFault> {
        let positions: Vec<usize> = self
            .operations
            .iter()
            .enumerate()
            .filter(|(_, operation)| !operation.cascade.is_empty() && !operation.kind.cascades())
            .map(|(position, _)| position)
            .collect();
        (!positions.is_empty()).then(|| PlanFault::misplaced_cascade(positions))
    }

    /// The fault of a resolved plan writing a vault control file beside an
    /// operation on documents, as an authored plan's is judged
    /// ([`AuthoredPlan::control_files_beside_documents`]).
    pub fn control_files_beside_documents(&self) -> Option<PlanFault> {
        control_files_beside_documents(&self.operations)
    }

    /// The plan carrying `footnote`.
    #[must_use]
    pub fn with_footnote(mut self, footnote: impl Into<String>) -> Self {
        self.footnote = Some(footnote.into());
        self
    }
}

/// A plan a caller sends: operations it authored, or a resolved plan.
///
/// On the wire a document is the plan itself, which names itself under
/// `plan`: `{"plan":"operations","vault":…,"operations":[…]}`,
/// `{"plan":"resolved","vault":…,"root":…,…}`. A resolved plan taken
/// unchanged out of any answer is a document. A key the plan does not name is
/// refused, at every depth.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlanDocument {
    /// Operations, planned and applied in one request. Sending them again is
    /// a new change.
    Operations(AuthoredPlan),
    /// A resolved plan, applied where every target holds its before-state or
    /// its after-state and every condition holds.
    Resolved(ResolvedPlan),
}

impl PlanDocument {
    /// The document carrying the operations of `plan`.
    pub const fn operations(plan: AuthoredPlan) -> Self {
        PlanDocument::Operations(plan)
    }

    /// The document carrying the resolved `plan`.
    pub const fn resolved(plan: ResolvedPlan) -> Self {
        PlanDocument::Resolved(plan)
    }

    /// The vault the plan is for.
    pub const fn vault(&self) -> &VaultAddress {
        match self {
            PlanDocument::Operations(plan) => &plan.vault,
            PlanDocument::Resolved(plan) => &plan.vault,
        }
    }
}

impl Serialize for PlanDocument {
    /// A document is written as the plan it holds, which carries its own tag.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            PlanDocument::Operations(plan) => plan.serialize(serializer),
            PlanDocument::Resolved(plan) => plan.serialize(serializer),
        }
    }
}

/// The name under a document's `plan`: which of the two plans it is.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PlanName {
    Operations,
    Resolved,
}

/// A document as it arrives: its `plan` tag and every field either plan
/// names, and no other. A field only one plan names is held as whether it was
/// written, so the plan the tag names can refuse one that belongs to the
/// other.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentFields {
    plan: PlanName,
    vault: VaultAddress,
    operations: Vec<Operation>,
    #[serde(default, deserialize_with = "written")]
    root: Option<RootIdentity>,
    #[serde(default, deserialize_with = "written")]
    transitions: Option<Vec<Transition>>,
    #[serde(default, deserialize_with = "written")]
    conditions: Option<Vec<PlanCondition>>,
    #[serde(default, deserialize_with = "written")]
    provenance: Option<Option<Provenance>>,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    footnote: Option<String>,
}

/// Whether a flag is left out of a plan's or a write request's bytes:
/// `false`, which is what its absence reads as. serde hands the field by
/// reference.
pub(crate) const fn is_false(flag: &bool) -> bool {
    !*flag
}

/// A field an operation list does not take, refused where it was written.
fn not_an_operations_field<E: serde::de::Error>(name: &str, written: bool) -> Result<(), E> {
    if written {
        return Err(E::custom(format_args!(
            "an `operations` plan does not take `{name}`: it is a field of a `resolved` plan"
        )));
    }
    Ok(())
}

impl<'de> Deserialize<'de> for PlanDocument {
    /// Every key is read by the derive — refusing any neither plan names and
    /// any written twice, in whatever order they are written — and the plan
    /// the `plan` tag names is then built from the fields it takes, refusing
    /// a field only the other plan names and requiring each it names.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let DocumentFields {
            plan,
            vault,
            operations,
            root,
            transitions,
            conditions,
            provenance,
            force,
            footnote,
        } = DocumentFields::deserialize(deserializer)?;
        match plan {
            PlanName::Operations => {
                not_an_operations_field("root", root.is_some())?;
                not_an_operations_field("transitions", transitions.is_some())?;
                not_an_operations_field("conditions", conditions.is_some())?;
                not_an_operations_field("provenance", provenance.is_some())?;
                Ok(PlanDocument::Operations(AuthoredPlan {
                    plan: OperationsTag,
                    vault,
                    operations,
                    force,
                    footnote,
                }))
            }
            PlanName::Resolved => Ok(PlanDocument::Resolved(ResolvedPlan {
                plan: ResolvedTag,
                vault,
                root: root.ok_or_else(|| D::Error::missing_field("root"))?,
                operations,
                transitions: transitions.ok_or_else(|| D::Error::missing_field("transitions"))?,
                conditions: conditions.ok_or_else(|| D::Error::missing_field("conditions"))?,
                provenance: provenance.flatten(),
                force,
                footnote,
            })),
        }
    }
}

impl JsonSchema for PlanDocument {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("PlanDocument")
    }

    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("norn_wire::PlanDocument")
    }

    /// One of the two plans, each advertising its own tag, so the schema a
    /// document is validated against is the schema of the plan an answer
    /// carries.
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let operations = generator.subschema_for::<AuthoredPlan>();
        let resolved = generator.subschema_for::<ResolvedPlan>();
        json_schema!({
            "description": "A plan a caller sends: operations it authored, or a resolved plan. A document is the plan itself, which names itself under `plan`, so a resolved plan taken unchanged out of any answer is a document. A key the plan does not name is refused, at every depth.",
            "oneOf": [operations, resolved],
        })
    }
}
