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
//! a file's content, one entry of the plan's resolution change set, or a
//! link's address resolving as recorded at the after-state — checked when it
//! is applied; and [`Provenance`] records what a plan was
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
//! **A control-file plan changes nothing else** (ADR 0037): a plan writing a
//! vault control file beside an operation on documents is a fault in its
//! shape, judged by [`AuthoredPlan::control_files_beside_documents`] and
//! [`ResolvedPlan::control_files_beside_documents`].
//!
//! **Provenance is what a repair plan was planned from.** Repair plans cite
//! the finding generation they read, the findings each operation fixes, the
//! findings they skipped and where the next batch continues. `Host::repair`
//! is the one handler that emits it, and only on a resolved plan: an authored
//! plan plans from no findings. It is a record, never checked when the plan is
//! applied, and the fresh plan a refusal answers carries none. It is spelled
//! here so a repair plan is the same document every other plan is.
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
use crate::cursor::Cursor;
use crate::document::{DocumentPath, LinkFamily, written_protocol};
use crate::finding_row::{
    CandidateHead, RequiredFieldHead, ValueCandidateHead, ValueHead, plan_candidate_head,
    plan_candidate_head_schema, plan_optional_value_head, plan_optional_value_head_schema,
};
use crate::plan::hash::ContentHash;
use crate::plan::operation::{Operation, OperationId, OperationKind, written};
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
/// address a rewrite replaced, a link in a removed document — is no entry of
/// the change set: its disappearance is the plan's own transition, guarded by
/// that file's hashes. A key can still name such an address, as an address
/// resolution about an address a repair replaces does.
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

    /// The protocol the address is written with, and the stem after it:
    /// `vault://notes/a` is `(Some("vault"), "notes/a")`, and `notes/a` is
    /// `(None, "notes/a")`. A protocol is recognized as the text layer
    /// recognizes one, so `HTTPS://x` and `note:draft` are stems with none.
    pub fn protocol_and_stem(&self) -> (Option<&str>, &str) {
        match written_protocol(&self.address) {
            Some(protocol) => (
                Some(protocol),
                &self.address[protocol.len() + "://".len()..],
            ),
            None => (None, &self.address),
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
/// **Three kinds of fact.** A content hash is a fact about a file the plan does
/// not write. A link resolution is one entry of the plan's resolution change
/// set: how one link resolves with every target of the plan at its
/// before-state, and with every target at its after-state, so a plan's own
/// progress never changes it. Its link may sit in a file the plan writes — a
/// link a cascade rewrites has an entry — so it is not a fact about a file
/// the plan leaves alone. An address resolution is the third kind: a fact a
/// repair's planner records about a link it writes or an address it replaces,
/// checked at the after-state and outside the change set.
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
/// holds that the plan does not record. An address resolution is not an
/// entry of the set: it is never computed again from the operations, and the
/// comparison ignores it.
///
/// **An address resolution is checked at the after-state.** From this holder,
/// this address must resolve to `after` once the plan has been applied, in a
/// preview and an apply alike; the address need not stand in the holder after
/// the plan, since a repair records one it replaces.
///
/// On the wire a condition is an object tagged `condition`:
/// `{"condition":"content_hash","path":"notes/c.md","hash":"sha256:…"}`,
/// `{"condition":"link_resolution","link":{…},"before":{"resolves":"one","path":"a.md"},"after":{"resolves":"none"}}`,
/// `{"condition":"address_resolution","link":{…},"after":{"resolves":"one","path":"a.md"}}`.
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
    /// From the link's holder, the link's address resolves as recorded at the
    /// plan's after-state: a fact outside the resolution change set, which is
    /// never computed again from the operations.
    AddressResolution {
        /// The link whose address is checked, keyed by its holder, syntax and
        /// address.
        link: LinkKey,
        /// What the address resolves to from the holder with every target of
        /// the plan at its after-state.
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

    /// From the holder of `link`, its address resolves to `after` at the
    /// plan's after-state.
    pub const fn address_resolution(link: LinkKey, after: Resolves) -> Self {
        PlanCondition::AddressResolution { link, after }
    }
}

/// How sure a repair is of the value it writes. The levels run strongest
/// first, so a smaller level is a stronger one.
///
/// On the wire a confidence is the flat string itself: `"declared"`,
/// `"derived"`, `"suggested"`.
///
/// **A threshold admits its own level and every stronger one.** A request
/// naming a threshold of `derived` admits `declared` and `derived` and not
/// `suggested`; `suggested` admits all three; `declared` admits only itself.
/// A request naming none is read at `derived`.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Confidence {
    /// The vault's schema names this value as the fix.
    Declared,
    /// The value follows from the document and the vault's rules by one
    /// determined step.
    Derived,
    /// The value is a guess a person should look at.
    Suggested,
}

impl Confidence {
    /// The threshold a request that names none is read at.
    pub const DEFAULT_THRESHOLD: Confidence = Confidence::Derived;

    /// Whether this threshold admits `level`: its own level and every
    /// stronger one.
    pub fn admits(self, level: Confidence) -> bool {
        level <= self
    }
}

/// Why a repair plan left a finding alone. The reasons are closed: each is a
/// decision the repair made, and a reason a build does not know is a read
/// refused.
///
/// On the wire a reason is the flat string itself: `"no_declared_fix"`,
/// `"below_threshold"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SkipReason {
    /// No rule declares a fix for the finding.
    NoDeclaredFix,
    /// Candidate values tie at the level that decides between them.
    Tie,
    /// The fix is below the request's confidence threshold.
    BelowThreshold,
    /// The document cannot be read.
    Unreadable,
    /// The rules declare conflicting defaults for the field.
    ConflictingDefaults,
    /// The fix would bring in required fields the document does not hold.
    BringsInRequiredFields,
    /// A capture in the fix's rule matches more than one way.
    AmbiguousCapture,
    /// The rules selecting the document conflict over where it may stand.
    RulesConflict,
    /// The fix would rename a field onto one the document already holds.
    RenameOntoOccupiedField,
    /// The link's address resolves to more than one document.
    AmbiguousLink,
    /// The document belongs to a class a repair does not touch.
    ExcludedClass,
    /// The write would be refused when the plan is applied.
    JudgeWouldRefuse,
    /// Something already stands where the fix would write.
    DestinationTaken,
}

/// The candidates a skipped finding had to choose between: documents, or
/// values.
///
/// On the wire the candidates are an object tagged `of`:
/// `{"of":"documents","head":{"candidates":[…],"total":9}}`,
/// `{"of":"values","head":{"candidates":[…],"total":2}}`. Either head is
/// bounded, and carries how many there were.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "of", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum SkippedCandidates {
    /// The documents a link's address could have named.
    #[non_exhaustive]
    Documents {
        /// The head of the documents.
        #[serde(deserialize_with = "plan_candidate_head")]
        #[schemars(schema_with = "plan_candidate_head_schema")]
        head: CandidateHead,
    },
    /// The values a field could have been given.
    #[non_exhaustive]
    Values {
        /// The head of the values, each with the rule that proposed it.
        head: ValueCandidateHead,
    },
}

impl SkippedCandidates {
    /// The documents `head` carries.
    pub const fn documents(head: CandidateHead) -> Self {
        SkippedCandidates::Documents { head }
    }

    /// The values `head` carries.
    pub const fn values(head: ValueCandidateHead) -> Self {
        SkippedCandidates::Values { head }
    }
}

/// A finding a repair plan left alone, and why.
///
/// On the wire the parts that are absent are left out:
/// `{"finding":42,"reason":"below_threshold","value":{…},"proposed":{…}}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkippedFinding {
    /// The finding's identity in the vault's findings.
    pub finding: u64,
    /// Why the plan left it alone.
    pub reason: SkipReason,
    /// The finding's actual value, as its bounded head, and absent where the
    /// finding is about none: a missing required field.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "plan_optional_value_head"
    )]
    #[schemars(schema_with = "plan_optional_value_head_schema")]
    pub value: Option<ValueHead>,
    /// What the plan had to choose between, where the reason is a choice it
    /// would not make.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates: Option<SkippedCandidates>,
    /// The fields the fix would bring in, where the reason is that it brings
    /// in required fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_fields: Option<RequiredFieldHead>,
    /// The operation the plan would have held had the proposal cleared its
    /// threshold, where the reason is that it did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed: Option<Operation>,
    /// Words about the skip, for a person reading the plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl SkippedFinding {
    /// The finding `finding`, left alone for `reason`, with no further
    /// detail.
    pub const fn new(finding: u64, reason: SkipReason) -> Self {
        SkippedFinding {
            finding,
            reason,
            value: None,
            candidates: None,
            required_fields: None,
            proposed: None,
            note: None,
        }
    }

    /// The skip over the finding's actual value `value`.
    #[must_use]
    pub fn with_value(mut self, value: ValueHead) -> Self {
        self.value = Some(value);
        self
    }

    /// The skip choosing between `candidates`.
    #[must_use]
    pub fn with_candidates(mut self, candidates: SkippedCandidates) -> Self {
        self.candidates = Some(candidates);
        self
    }

    /// The skip whose fix would bring in the fields `required_fields`.
    #[must_use]
    pub fn with_required_fields(mut self, required_fields: RequiredFieldHead) -> Self {
        self.required_fields = Some(required_fields);
        self
    }

    /// The skip of a proposal that would have been the operation `proposed`.
    #[must_use]
    pub fn with_proposed(mut self, proposed: Operation) -> Self {
        self.proposed = Some(proposed);
        self
    }

    /// The skip noting `note`.
    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

/// One finding an operation fixes, and how sure the repair is of the fix.
///
/// On the wire: `{"finding":42,"value":{…},"confidence":"derived","notes":[…]}`;
/// `value` is left out where the finding is about none.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CitedFinding {
    /// The finding's identity in the vault's findings.
    pub finding: u64,
    /// The finding's actual (offending) value, as its bounded head, and
    /// absent where the finding is about none: a missing required field.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "plan_optional_value_head"
    )]
    #[schemars(schema_with = "plan_optional_value_head_schema")]
    pub value: Option<ValueHead>,
    /// How sure the repair is of the fix.
    pub confidence: Confidence,
    /// Words about the fix, for a person reading the plan.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl CitedFinding {
    /// The finding `finding`, fixed with `confidence`, about no value and
    /// with no note.
    pub const fn new(finding: u64, confidence: Confidence) -> Self {
        CitedFinding {
            finding,
            value: None,
            confidence,
            notes: Vec::new(),
        }
    }

    /// The citation over the finding's actual value `value`.
    #[must_use]
    pub fn with_value(mut self, value: ValueHead) -> Self {
        self.value = Some(value);
        self
    }

    /// The citation carrying `notes`.
    #[must_use]
    pub fn with_notes(mut self, notes: Vec<String>) -> Self {
        self.notes = notes;
        self
    }
}

/// The findings one operation of a repair plan fixes.
///
/// On the wire: `{"operation":"repair-1","findings":[…]}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Citation {
    /// The operation, by the identifier it carries in the plan. A repair plan
    /// numbers its operations `repair-1`, `repair-2`, and so on, in plan
    /// order.
    pub operation: OperationId,
    /// The findings the operation fixes: one operation can fix several.
    pub findings: Vec<CitedFinding>,
}

impl Citation {
    /// The operation `operation`, fixing `findings`.
    pub const fn new(operation: OperationId, findings: Vec<CitedFinding>) -> Self {
        Citation {
            operation,
            findings,
        }
    }
}

// `Host::repair` (Layer 5B, NORN-373) emits this: a repair plan cites the
// finding generation it read, the findings each operation fixes and the
// findings it skipped, and carries the cursor that continues a batch that
// leaves more. Its published description stays wire-facing, so the note
// lives here rather than in the doc comment schemars lifts.
/// What a repair plan was planned from. It is a record, never checked when
/// the plan is applied.
///
/// **A repair plans a batch.** It plans the findings of a run of documents,
/// in path order; `limit` is a soft target in selected findings, and a batch
/// extends through the last findings of its last document. The first batch
/// says how many selected findings remain after it; a batch that leaves more
/// carries the cursor that continues from the last document it covered, and
/// one that leaves none carries no cursor.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    /// The write generation of the findings the plan was planned from.
    pub finding_generation: u64,
    /// The findings each operation fixes, one entry for each operation that
    /// fixes any.
    pub citations: Vec<Citation>,
    /// The findings the plan left alone.
    pub skipped: Vec<SkippedFinding>,
    /// How many selected findings remain after this batch, exactly. Present
    /// on the first batch only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<u64>,
    /// Where the next batch continues: the cursor that follows the last
    /// document this batch covered. Present exactly when more findings
    /// remain.
    // Boxed so a resolved plan, which every answer to an apply carries, stays
    // small; the bytes are the cursor's either way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Box<Cursor>>,
}

impl Provenance {
    /// A plan planned from the findings at `finding_generation`, leaving
    /// `skipped` alone, citing no finding and leaving no more: no cursor.
    pub const fn new(finding_generation: u64, skipped: Vec<SkippedFinding>) -> Self {
        Provenance {
            finding_generation,
            citations: Vec::new(),
            skipped,
            remaining: None,
            cursor: None,
        }
    }

    /// The provenance citing `citations`.
    #[must_use]
    pub fn with_citations(mut self, citations: Vec<Citation>) -> Self {
        self.citations = citations;
        self
    }

    /// The provenance of a first batch, with `remaining` selected findings
    /// left after it.
    #[must_use]
    pub const fn with_remaining(mut self, remaining: u64) -> Self {
        self.remaining = Some(remaining);
        self
    }

    /// The provenance of a batch that leaves more, continued by `cursor`.
    #[must_use]
    pub fn continued_by(mut self, cursor: Cursor) -> Self {
        self.cursor = Some(Box::new(cursor));
        self
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
    /// 0037). A control file is what every document is judged under, so a
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
    /// What must still hold: the content of a file the plan does not write,
    /// an entry of the plan's resolution change set, and how an address
    /// resolves at the plan's after-state.
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
