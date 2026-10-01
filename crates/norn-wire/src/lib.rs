#![forbid(unsafe_code)]
//! The vocabulary. Pure types: no I/O and no effects.
//!
//! What crosses the client/host seam is defined here exactly once, and every
//! surface is a derived rendering of it: CLI flags, MCP tool schemas and HTTP
//! payloads render these types and never define vocabulary of their own. The
//! crate links nothing else in the workspace and reaches no filesystem, no
//! database and no socket — a type in here can be constructed, serialized and
//! compared, and nothing it does is visible anywhere but in the value it
//! hands back.
//!
//! **The logic here is the vocabulary's own grammar, never a vault's or a
//! store's.** Parsing a name, a path, a resolution target or a cursor;
//! rendering a cursor; judging what moved under a continuation; sorting the
//! names a refusal carries — each of those is a fact about how the vocabulary
//! is spelled, decidable from the value alone. What a vault holds, what a
//! store has derived and what a host is serving are read nowhere in here.
//!
//! What is defined here today is where a vault entry stands — [`TrustState`]
//! and the [`UntrustedReason`] it carries, read a second way as [`NotReady`]
//! by a request that cannot answer with a state — the one shape a refusal
//! takes: [`ErrorEnvelope`], with its [`ReasonCode`] and [`ErrorDetail`]; and
//! what a finding is filed under, [`FindingKind`], with the [`FindingScope`]
//! its kind answers and its [`Severity`]. Maintainer
//! contention carries the diagnostic [`MaintainerIdentity`] reported by the
//! lock without changing an entry's trust state. Requests are spelled here
//! too: the [`VaultAddress`] a request names its vault by — a [`VaultName`] or
//! a [`VaultRoot`] — the [`AttachMode`] a demand asks for its derived state
//! under, the [`Verb`] a request asks for with the [`Addressing`] it carries a
//! vault by and the [`RequestScope`] that addressing is answered from, the
//! [`Predicate`] list a read filters by, whose path part is a glob read by the
//! [`Pattern`] grammar a schema's path and tag sets are written in too, and
//! whose tag part compares under the one tag fold, [`fold_tag`], and
//! the [`ResolutionTarget`] one
//! document is addressed by. What a read answers with is spelled here as
//! well: the [`AnswerReading`] every answer carries — its [`TrustState`] and
//! its establishment — the [`Unsatisfied`] parts of a request that could not be applied, the
//! [`AnswerAdvisory`] saying what an applied part assumed or where it stopped,
//! with the [`ComparedBy`] place a comparison compared in, the
//! [`RungSkipReason`] a rung was left out for, and the [`RUNG_DEPTH`] a rung
//! stops at, and
//! the [`Cursor`] a page continues from, with the [`Snapshot`] a continuation
//! is judged against, what [`Moved`] under it, and the [`CursorKey`] each
//! paged row type stops at.
//!
//! The six read verbs are spelled here as one params type and one report type
//! each: [`FindParams`] answering [`FindReport`], [`SearchParams`] answering
//! [`SearchReport`] over the [`RungSelection`] it asks for, declaring the
//! [`LadderDeclaration`] of [`RungReport`]s it ran beside the [`Hit`]s it
//! ranked,
//! [`GetParams`] answering [`GetReport`], [`CountParams`] answering
//! [`CountReport`] of [`Tally`]s, [`ValidateParams`] answering
//! [`ValidateReport`], and [`DescribeParams`] answering [`DescribeReport`] of
//! [`Facet`]s, one of which reports the [`TagStance`] a vault takes on a tag
//! it did not declare. What they page is spelled here too: the
//! [`DocumentRow`] a [`Column`] projection selects, at its [`DocumentPath`],
//! with the [`LinkRow`], [`HeadingRow`], [`BlockRow`], [`TagRow`] and
//! [`FindingRow`] its [`Collection`]s hold, the [`BodyText`] its body crosses
//! as, and the [`FieldValue`] each frontmatter key carries. A link row, a
//! finding and an ambiguous-target refusal each carry a [`CandidateHead`] —
//! at most [`CANDIDATE_HEAD`] [`Candidate`]s and the total they head — and
//! the latter two carry it beside the [`Hint`] that names what enumerates the
//! rest. What a reload met is spelled
//! here too:
//! [`ReloadFailure`], with the [`ControlFileFailure`] — a [`ControlFile`] and
//! the [`ReloadStage`] it refused at — that three of its carriers hold alone.
//! [`Directory`] is a directory a client asks a question about, the third
//! path grammar beside [`VaultRoot`] and [`SchemaSource`]. [`EngineSection`]
//! is what a host was delivered as a vault's engine section, which is what a
//! status answer reports and what a vector refusal is composed against.
//!
//! The seven verbs of the vault namespace and
//! [`doctor`](DoctorRegistryParams)'s registry half are spelled the same
//! way: [`RegisterParams`] answering
//! [`RegisterReport`], [`UnregisterParams`] answering [`UnregisterReport`],
//! [`ListParams`] answering [`ListReport`], [`VaultSetParams`] — whose every field
//! is a [`VaultChange`], or a [`VaultReplace`] where the field has no default to be
//! cleared to — answering [`VaultSetReport`], [`ResolveParams`] answering
//! [`ResolveReport`], [`StatusParams`] answering [`StatusReport`],
//! [`ReloadParams`] answering [`ReloadReport`] of a [`ReloadOutcome`], and
//! [`DoctorRegistryParams`] answering [`DoctorRegistryReport`] over the
//! [`RegistrySanity`] of the registry itself, its [`RegistryProblem`]s and the
//! [`EngineHealth`] of each vault. What those eight report about a
//! registration is spelled here too: the [`Registration`] itself, the
//! [`Published`] answer an entry carries, the [`Fingerprints`] it serves
//! under, the [`Drift`] its authored control files stand at, the
//! [`EngineStatus`] of its engine slot, the [`Advisory`] list its serving
//! raises, and the [`VaultStatus`] that holds all of them — with the [`RollUp`]
//! those statuses add up to and the [`Attention`] it names them for.
//!
//! The seven write verbs are spelled the same way, and all seven answer
//! [`ApplyReport`]. `apply` carries [`ApplyParams`] — an [`ApplyMode`] and a
//! [`PlanDocument`]. A plan
//! document is an [`AuthoredPlan`] of [`Operation`]s, each an
//! [`OperationKind`] with the [`OperationId`] it is required by and the
//! [`AuthorCondition`]s its author observed — a content hash, or the
//! [`ExpectedField`] a frontmatter field held — or a [`ResolvedPlan`]: the
//! [`RootIdentity`] it was resolved against, one [`Transition`] per file
//! between two [`FileState`]s, each absent or a [`ContentHash`], the
//! [`PlanCondition`]s its planning read — a file's content, or what one link,
//! named by its [`LinkKey`], [`Resolves`] to before the plan and after it —
//! and the [`Provenance`] a repair plan cites, with the [`SkippedFinding`]s
//! it left alone. In a resolved plan an operation that moves, removes or
//! retargets documents carries its link cascade, one [`LinkRewrite`] per
//! document, syntax and address among the links it changes, and a delete
//! says what becomes of the links naming its document as its [`Backlinks`].
//! Either plan carries whether it is
//! forced past the schema check. A frontmatter kind names its documents by a
//! [`WriteTarget`] and writes an [`AuthoredValue`] — a [`FiniteFloat`] or a
//! [`ValueMap`] among its shapes. The other write verbs each compile to an
//! authored plan: `set` from [`SetParams`] of [`FieldChange`]s, `edit` from
//! [`EditParams`] of [`DocumentEdit`]s, `new` from [`NewParams`], `move` from
//! [`MoveParams`] of a [`MoveSubject`] — a document or a folder, read from
//! the source, or the [`IllegalMove`] two ends name — `delete` from
//! [`DeleteParams`], and `rewrite_wikilink` from [`RewriteWikilinkParams`].
//! A preview answers with
//! the resolved plan and a [`Forecast`] of what the plan does not carry — the
//! targets that drifted, the [`FolderPath`]s a plan makes and removes, each
//! [`SchemaViolation`] a force lets through, a [`LinkAdvisory`] for each link
//! a caller should look at, and the [`FilePath`] of each file a folder move
//! leaves behind; an
//! applied plan with its [`ChangesetOutcome`], the
//! [`TargetResult`] of each [`AppliedTarget`] and what it forced. An apply
//! that ends any other
//! way ends in a code, whose detail carries the [`RefusedCheck`]s and the
//! [`UnresolvedOperation`]s — each with its [`UnresolvedReason`] — of a
//! refusal, the [`InterruptionCause`] of an interruption and what its force
//! let through in what landed, or the [`PlanFault`]
//! of a plan whose own shape is wrong.
//!
//! **A registry report and a lifecycle observation carry no answer reading.**
//! A registration is not answered from a database, and neither a status nor a
//! reload is a read: a status is taken off what an entry already publishes and
//! creates no demand, and a reload reports what it applied. So the eight
//! reports above cross as themselves rather than inside a [`VaultAnswer`],
//! which is what carries an [`AnswerReading`] for the six verbs that do read
//! and for `apply`, whose plan is resolved against a snapshot as a read's
//! answer is.
//!
//! Nothing crosses the seam that is not a type from here. There is no untyped
//! JSON value in any signature and no JSON-in-a-string; a payload that cannot
//! be spelled as a type here does not cross.
//!
//! # Derive discipline
//!
//! Every public type that crosses the seam carries the same derives, and the
//! reason is that a wire type is read by serde and described by schemars at
//! once — a type that serializes but has no schema is a payload no surface can
//! advertise. [`Pattern`] and the refusals a grammar's constructor returns are
//! readings of a value rather than values that cross, and carry none of them:
//!
//! - [`serde::Serialize`] and [`serde::Deserialize`], with `snake_case` field
//!   and variant names on the wire — except the code registries, whose members
//!   are renamed to the `namespace/what-happened` grammar below. A grammar's
//!   read path is written by hand where the read is the constructor —
//!   [`VaultName`], [`VaultRoot`], [`SchemaSource`], [`DocumentPath`],
//!   [`FolderPath`], [`ResolutionTarget`], [`Directory`], [`ContentHash`],
//!   [`RootIdentity`], [`OperationId`] and [`Score`] refuse a string outside
//!   their grammar, [`FiniteFloat`] refuses `NaN` and an infinity,
//!   [`AuthoredValue`] and [`ValueMap`] read any plain value through that same
//!   refusal and refuse a map key written twice,
//!   [`RungSet`] refuses a ladder that runs no rung, [`RegistrySanity`]
//!   refuses a problem list that names no problem and [`NameSet`] refuses a
//!   name list naming fewer than two distinct names, and five shapes refuse a
//!   value whose halves disagree: [`ErrorEnvelope`], whose code must be its
//!   detail's, [`LinkRow`], whose health must be the health of the total
//!   documents its head heads, [`RollUp`], whose five counts must sum to the
//!   vaults it says it counted, and the three bounded heads — [`Collection`],
//!   [`BodyText`] and [`CandidateHead`] — whose total must be a total the
//!   head they carry can head, the last of them refusing a head wider than
//!   [`CANDIDATE_HEAD`] as well — each with the wire shape a derive would
//!   read. [`Operation`]'s read path, and [`OperationKind`]'s alone, are
//!   written by hand because the derive
//!   cannot refuse what it must: the derive reads it into a private shape
//!   that refuses any key it does not name and holds every field any kind
//!   names, and the kind then takes the fields it names, refusing one it lacks
//!   and one it does not take. A frontmatter kind's [`WriteTarget`] is two
//!   keys among those fields, exactly one of them written, so the target is
//!   written by hand on both sides too, and [`SetParams`], which holds a
//!   target among its own keys, is read the way an operation is.
//!   A delete's [`Backlinks`] is likewise at most one of two keys among the
//!   fields that name the delete, so it is written by hand, and
//!   [`DeleteParams`] reads its two keys the way an operation does.
//!   [`MoveParams`] reads its two ends together, as text, before deciding
//!   which grammar reads them, and a cascade's [`LinkRewrite`] holds its two
//!   addresses to the rule a `rewrite_link` operation's are held to, so each
//!   is read by the derive into a private shape first.
//!   [`Cursor`] and [`PlanDocument`] are written by
//!   hand on both sides: a cursor's wire shape is one opaque string rather
//!   than the fields a derive would emit, and a document is written as the
//!   plan it holds and read by the derive into one private shape holding its
//!   `plan` tag and every field either plan names, from which the plan the
//!   tag names is built. Neither plan read holds what it reads in a buffer: a
//!   buffer keeps the last of two values written for one key where the derive
//!   refuses the second, so a plan read inside a request would read otherwise
//!   than the plan alone. The cursor's rendering is the one runtime use of
//!   `serde_json` here, and it is in no signature. The plan tags
//!   [`OperationsTag`] and [`ResolvedTag`] are written by hand on both sides
//!   too: each is a zero-sized marker, written as its constant and read as a
//!   one-member vocabulary that refuses any other value.
//! - [`schemars::JsonSchema`], which reads the same serde attributes, so the
//!   advertised schema and the emitted bytes are one description. It too is
//!   written by hand where a derive would advertise a shape the reader does
//!   not accept. These types do: the grammars [`VaultName`], [`VaultRoot`],
//!   [`SchemaSource`], [`Directory`], [`DocumentPath`], [`FolderPath`],
//!   [`FilePath`], [`ResolutionTarget`], [`ContentHash`], [`RootIdentity`] and
//!   [`OperationId`] advertise the pattern or floor their constructors
//!   hold; [`Operation`] advertises its kind's own branches with its optional
//!   parts added inside each, since each branch refuses a key it does not
//!   name; [`WriteTarget`] advertises its two keys with exactly one required,
//!   and [`OperationKind`] and [`SetParams`] keep the derive but settle
//!   the target flattened into them as an object whose keys are all its own;
//!   [`Backlinks`] advertises its two keys and states as a `not` the pair its
//!   reader refuses, and [`OperationKind`] and [`DeleteParams`] settle it the
//!   same way; a wikilink rewrite's ends and a delete's `rewrite_to` advertise
//!   a target with no `#`, since each names a whole document;
//!   [`MoveParams`] advertises the private shape it is read through, its two
//!   ends the paths they are written as;
//!   [`AuthoredValue`] and [`ValueMap`] are a tree of plain values, stating in
//!   words the finiteness and single keys JSON Schema cannot;
//!   [`FiniteFloat`] is a number;
//!   [`PlanDocument`] is one of the two plan types, each advertising
//!   its own tag, and the tags [`OperationsTag`] and [`ResolvedTag`] are each
//!   their constant inline; [`Cursor`] is one opaque string
//!   rather than the fields a derive would emit; [`RungSet`],
//!   [`RegistrySanity`] and [`NameSet`] carry the
//!   `minItems` floor their read paths keep, the last of them advertising
//!   `uniqueItems` for the distinctness it is measured against as well;
//!   [`CandidateHead`] carries the
//!   `maxItems` ceiling its read path keeps, read off [`CANDIDATE_HEAD`] so
//!   the bound has one spelling; [`LadderDeclaration`] advertises that its
//!   rungs contain a retrieval rung; and [`RungSubtraction`] advertises its
//!   rungs each once and never every retrieval rung.
//! - `Debug`, `Clone` and `PartialEq`, plus `Eq` wherever every field holds it.
//!   [`FiniteFloat`] holds it by construction, since the one value that breaks
//!   a float's equality cannot be built, so a plan carrying a written float is
//!   `Eq` like every other plan.
//!
//! **Enums are internally tagged with an explicit tag name, never externally
//! tagged.** An externally tagged enum makes the variant name a JSON key, so a
//! reader has to enumerate keys to learn what it is holding and a new variant
//! changes the object's shape rather than one field's value. The tag names are
//! part of the wire: [`TrustState`] is tagged `state`, [`UntrustedReason`] is
//! tagged `kind`, and [`ErrorDetail`] is tagged `code`.
//!
//! **A tagged enum's variants are struct-shaped.** Internal tagging merges the
//! tag into the variant's own map, so a newtype variant fails at serialize
//! time while schemars advertises a schema saying it works: the break arrives
//! at runtime, against a shape a consumer was told to expect. A variant that
//! carries data names its fields.
//!
//! **Two plan shapes carry their tag another way.** [`OperationKind`] is
//! adjacently tagged — its kind under `kind`, its fields under `fields` —
//! because that is the shape an operation's author writes, and its fields are
//! structs the tag never enters. [`AuthoredPlan`] and [`ResolvedPlan`] each
//! carry their own `plan` tag as a field, so a resolved plan written inside
//! any answer is the bytes a caller sends back; [`PlanDocument`] is written as
//! the plan it holds and read by dispatching on that tag, rather than being a
//! serde-tagged enum that would consume the tag before its plan read it.
//!
//! **Two write shapes carry no tag at all.** An [`AuthoredValue`] is the
//! plain JSON value of its shape, because the shape already says what it is
//! and an author writes a field the way the field reads in a document. A
//! [`WriteTarget`] is exactly one of two keys, `path` or `where`, among the
//! fields that name it, because `path` is the key every other kind names its
//! document by; which key is written is what the target is.
//!
//! **A tagged object or a flat string** is decided by whether the variants
//! carry data. A closed vocabulary whose members carry nothing is a flat
//! string, as [`ReasonCode`], [`FindingKind`], [`Severity`], [`AttachMode`],
//! [`WatcherLossCause`] and [`WarmingPhase`] are; an enum whose variants may
//! carry a payload is a
//! tagged object, as [`TrustState`], [`UntrustedReason`] and [`ErrorDetail`]
//! are. The flat string keeps a code matchable as a value; the tagged object
//! keeps a payload's arrival from changing what the value is. Two of those
//! flat strings are code registries; [`WatcherLossCause`] and
//! [`WarmingPhase`] are not codes but nested bare-string values under `cause`
//! and `phase`, carrying no namespace, and [`AttachMode`] is a request
//! parameter a demand carries in rather than anything a refusal is filed
//! under. [`VaultName`] is a flat string of a third kind: not a vocabulary at
//! all but a grammar, whose read path is its constructor, so a name that
//! crossed is a name that parsed.
//!
//! **Doc comments on a type, a variant or a field are published.** schemars
//! lifts them verbatim into the schema `description`s an MCP consumer reads,
//! so they carry wire documentation and nothing else: no Rust intralinks, no
//! maintainer rationale, no narration of shapes the type does not have.
//! Rationale belongs in module documentation such as this, which schemars does
//! not lift.
//!
//! # Extension, and what a version skew does
//!
//! Public enums are `#[non_exhaustive]`, save the plain ones below, and so is
//! [`ErrorEnvelope`], which extends by gaining a field. A variant that carries
//! a payload is `#[non_exhaustive]` in its own right, save the plan's below,
//! so the payload extends by gaining a field too rather than by breaking every
//! caller that destructured it.
//! `#[non_exhaustive]` binds across crates, so a shape consumers cannot write
//! as a literal carries a constructor: [`ErrorEnvelope::new`],
//! [`TrustState::warming`], [`TrustState::untrusted`],
//! [`UntrustedReason::watcher_lost`],
//! [`UntrustedReason::environmental_refusal`],
//! [`UntrustedReason::store_damaged_rebuilding`],
//! [`UntrustedReason::store_damaged_awaiting_demand`],
//! [`UntrustedReason::schema_unreadable`],
//! [`UntrustedReason::leg_unwound`],
//! [`NameSet::new`], [`ErrorDetail::duplicate_root`],
//! [`ErrorDetail::entry_untrusted`], [`ErrorDetail::maintainer_contended`],
//! [`ErrorDetail::unknown_vault`], [`ErrorDetail::unsupported_attach_mode`],
//! [`ErrorDetail::already_served`], [`ErrorDetail::entry_held`],
//! [`ErrorDetail::entry_not_ready`], [`ErrorDetail::reader_unavailable`],
//! [`ErrorDetail::registry_unwritable`],
//! [`ErrorDetail::ambiguous_root`], [`ErrorDetail::ambiguous_target`],
//! [`ErrorDetail::unknown_target`], [`ErrorDetail::reload_busy`],
//! [`ErrorDetail::reload_failed`], [`ErrorDetail::cursor_order_changed`],
//! [`ErrorDetail::engine_not_enabled`], [`ErrorDetail::engine_unavailable`],
//! [`ErrorDetail::engine_failed`], [`ErrorDetail::apply_not_run`],
//! [`ErrorDetail::apply_outcome_unknown`], [`ErrorDetail::plan_refused`],
//! [`ErrorDetail::root_changed`], [`ErrorDetail::plan_interrupted`],
//! [`ErrorDetail::write_failed`], [`ErrorDetail::plan_invalid`],
//! [`NotReady::warming`], [`NotReady::unattached`],
//! [`VaultAddress::name`], [`VaultAddress::root`],
//! the constructor on each [`Predicate`], [`Anchor`], [`CursorKey`],
//! [`Unsatisfied`], [`AnswerAdvisory`], [`ReloadFailure`] and [`RungReport`] variant,
//! [`EngineSection::malformed`] and the constructors on each
//! [`RungSelection`] variant, whose variants are the parts of a plain enum
//! that extend by gaining a field,
//! [`Cursor::new`], [`Page::new`], [`Snapshot::new`], [`SidecarRevision::new`],
//! [`CursorOrderChanged::new`], [`CursorOrderChanged::in_orders`],
//! [`CursorOrderChanged::in_ladders`],
//! [`Score::new`],
//! [`RungSkipReason::unavailable`],
//! [`AnswerReading::new`],
//! [`LadderDeclaration::new`], [`LadderDeclaration::lexical`], [`SearchReport::new`],
//! [`ModelIdentity::new`], [`Freshness::trailing`], [`Freshness::rescanning`],
//! [`VaultAnswer::new`], [`VaultAnswer::with_advisories`],
//! [`MaintainerIdentity::named`] and
//! [`MaintainerIdentity::unknown`];
//! the constructor on each [`Column`], [`FieldValue`], [`SortKey`],
//! [`GroupKey`], [`Hint`], [`CollectionPage`], [`GetReport`],
//! [`ValidateReport`] and [`Facet`] variant,
//! [`Span::new`], [`Collection::new`], [`BodyText::new`], [`LinkRow::new`],
//! [`HeadingRow::new`],
//! [`BlockRow::new`], [`TagRow::new`], [`DocumentRow::new`],
//! [`DocumentPath::new`], [`Candidate::new`], [`CandidateHead::new`],
//! [`FindingRow::new`],
//! [`Sort::new`], [`RungSet::lexical`], [`RungSet::of`],
//! [`RungSubtraction::none`], [`RungSubtraction::of`], [`Hit::new`],
//! [`Tally::new`], [`KindTally::new`], and the `new` on each of the six read
//! params types;
//! [`Registration::new`], [`Published::state`], [`Published::parked`],
//! [`Fingerprints::new`], [`Drift::inactive`],
//! [`Drift::current`], [`Drift::reload_pending`], [`Drift::unreadable`],
//! [`EngineStatus::off`], [`EngineStatus::on`], [`EngineStatus::self_disabled`],
//! [`ControlFileFailure::new`],
//! the constructor on each [`Advisory`], [`Attention`], [`ResolveReport`],
//! [`StatusReport`], [`RegistrySanity`] and [`RegistryProblem`] variant,
//! [`VaultStatus::new`], [`RollUp::of`], [`VaultChange::keep`], [`VaultChange::set`],
//! [`VaultChange::clear`], [`VaultReplace::keep`], [`VaultReplace::set`],
//! [`EngineHealth::new`], and the `new` on each of the seven vault-namespace
//! params types, on [`DoctorRegistryParams`], and on each of their reports;
//! [`ContentHash::from_sha256`], [`ContentHash::new`],
//! [`RootIdentity::from_device_and_inode`], [`OperationId::new`],
//! [`FolderPath::new`], [`FilePath::new`], the constructor on each
//! [`RefusedCheck`], [`UnresolvedReason`], [`InterruptionCause`],
//! [`PlanFault`], [`LinkAdvisory`] and [`ApplyReport`] variant,
//! [`ApplyReport::with_forced`], [`Forecast::new`], [`Forecast::with_forced`],
//! [`Forecast::with_links`], [`Forecast::with_left_behind`],
//! [`SchemaViolation::new`], [`UnresolvedOperation::new`],
//! [`AppliedTarget::new`], [`ApplyParams::new`], the `new` on [`SetParams`],
//! [`EditParams`], [`NewParams`], [`MoveParams`], [`DeleteParams`] and
//! [`RewriteWikilinkParams`], [`MoveSubject::new`], and the constructor on
//! each [`FieldChange`], [`DocumentEdit`] and [`MoveSubject`] variant. The
//! plan types the applier destructures, below, can be written as literals and
//! keep their constructors all the same: [`Operation::new`], the constructor
//! on each [`OperationKind`], [`AuthorCondition`], [`ExpectedField`],
//! [`PlanCondition`], [`Resolves`], [`FileState`], [`WriteTarget`] and
//! [`PlanDocument`] variant, [`AuthoredValue::string`],
//! [`AuthoredValue::float`], [`AuthoredValue::list`], [`AuthoredValue::map`],
//! [`LinkRewrite::new`], [`LinkKey::new`], [`Transition::new`],
//! [`SkippedFinding::new`], [`Provenance::new`], [`AuthoredPlan::new`] and
//! [`ResolvedPlan::new`].
//!
//! **A closed vocabulary whose every reader must decide what a new member
//! means is plain rather than `#[non_exhaustive]`.** The two rules answer two
//! different questions. `#[non_exhaustive]` keeps a member's arrival from
//! breaking a caller that only reads; a plain enum makes that arrival break
//! every caller that *composes*, which is what a vocabulary wants when no
//! reader can carry on without deciding. [`EngineSection`],
//! [`FindingScope`], [`RungSelection`], [`ApplyMode`], [`PlanDocument`],
//! [`OperationKind`], [`WriteTarget`], [`Backlinks`], [`AuthoredValue`],
//! [`FileState`], [`AuthorCondition`], [`ExpectedField`], [`PlanCondition`]
//! and [`Resolves`] are the fourteen members of that class: a section composes with an engine's
//! own refusal to say what a client should do, a scope decides whether a
//! finding is withheld from a document row, a selection is resolved to the
//! ladder a search runs, and the one applier must decide what every mode,
//! plan document, operation kind, target, delete's links, written value,
//! file state, condition and link resolution means — whether a
//! request writes, how a document is planned, how a kind resolves into
//! transitions, which documents it writes and what it writes there, how a
//! state is verified, how a condition is checked — since a
//! request it cannot interpret must never apply as though it could, and a
//! mode it has not decided must never fall into "otherwise apply". A composer of
//! any of them that has not made the decision should fail to compile rather
//! than fall into a default arm, so none carries the attribute and a new
//! member is a deliberate break at every composition site. The two rules compose rather
//! than exclude: [`EngineSection::Malformed`] carries a payload, so the
//! variant is `#[non_exhaustive]` in its own right and grows by gaining a
//! field, while the enum around it stays plain and grows by breaking every
//! composer.
//!
//! **What the one applier interprets is exhaustively destructurable.** The
//! payload variants of [`OperationKind`], [`WriteTarget`], [`Backlinks`],
//! [`AuthoredValue`], [`FileState`], [`AuthorCondition`], [`ExpectedField`], [`PlanCondition`]
//! and [`Resolves`], and the structs [`Operation`], [`LinkRewrite`],
//! [`LinkKey`], [`Transition`], [`ResolvedPlan`], [`AuthoredPlan`],
//! [`Provenance`] and [`SkippedFinding`],
//! carry no `#[non_exhaustive]` and hold only public fields; a plan's `plan`
//! tag is a public zero-sized marker, [`OperationsTag`] or [`ResolvedTag`]. The
//! one applier decides what every field of a plan means, so a field added to
//! any of them must fail to compile where the applier destructures the plan,
//! rather than pass it under a `..` unread — a field the applier ignores
//! weakens a check as surely as a key a reader drops. Everything else a caller
//! reads stays `#[non_exhaustive]` and grows without breaking it; these grow by
//! breaking the applier, deliberately.
//!
//! **What `#[non_exhaustive]` protects is Rust destructuring, not a writer's
//! bytes.** A field added to a payload is a field the read path requires, so
//! JSON an older writer produced — `{"kind":"environmental_refusal"}` once
//! `detail` exists — fails the read as a missing field. Growth is a promise to
//! Rust callers and to readers of the payload a current writer emits;
//! compatibility with bytes an older writer produced is not promised before
//! 1.0.
//!
//! **A struct tolerates a field it does not know; an enum refuses a variant it
//! does not know.** A reader drops an unknown field, so a writer that gained
//! one is still read by a reader that has not. A reader handed a tag or a code
//! string it does not know fails the read instead: there is no
//! `#[serde(other)]` catch-all anywhere in the vocabulary, because a variant
//! nobody can interpret is a refusal to parse rather than a value to pass on
//! degraded. [`RungSelection`] is the one request shape that refuses a field
//! it does not know: its two selections hold disjoint fields, and a field of
//! the other one dropped on the way in would read a request that both names a
//! set and subtracts from one as a request that does only one of them.
//!
//! **A plan refuses a field it does not know, at every depth.** The plan
//! documents, their operations, each kind's fields, their cascades, their
//! conditions and the link keys and resolutions inside them, their
//! transitions and file states, and a plan's provenance all refuse an unknown
//! key, where every answer drops one. The divergence follows the direction a
//! plan flows: an answer flows out to a caller, where a dropped field loses a
//! fact the caller could not use, but a plan flows into the host, where a
//! dropped field — a newer caller's condition — would weaken a check without a
//! word. The refusal is the version-mismatch signal, and a plan carries no
//! version field because it is short-lived: a caller whose plan is refused
//! previews again under the build it is talking to. An answer that carries a
//! plan — a report, or a refusal's fresh plan — still drops a field it does
//! not know at its own level, and the plan inside it still refuses one. The
//! write requests — [`SetParams`], [`EditParams`], [`NewParams`],
//! [`MoveParams`], [`DeleteParams`] and [`RewriteWikilinkParams`], with their
//! changes and edits — refuse one too, since each becomes a plan and a field
//! dropped from it would be dropped from the plan. A [`LinkKey`] inside a
//! forecast's [`LinkAdvisory`] refuses one as well: it is the key a plan's
//! condition holds a link under, read as the plan reads it.
//!
//! # The code grammar, and what is not a code
//!
//! **A code is a flat `namespace/what-happened` string**, lowercase kebab-case
//! on both sides of one slash. Codes are what a client enumerates, switches on
//! and filters by, and they live in exactly two closed registries:
//! [`ReasonCode`] for what was refused, and [`FindingKind`] for what a finding
//! is filed under (`document/…`).
//!
//! [`ReasonCode`] holds four namespaces, and which one a code sits in is
//! decided by what the fact is about. `host/…` is a fact about the host's
//! serving of an entry: a name it does not hold, a name it already serves, an
//! entry that is held, warming, untrusted, or serving with its read seam
//! down, a registry file it could not write. `vault/…` is a fact about the
//! requested vault's content, its control files, or which vault a directory
//! is in at all: every outcome of a reload that ran is one of these —
//! `vault/reload-busy` included, because what is busy is the work over that
//! vault rather than the host — and so is a directory more than one
//! registration contains, which names no one vault to answer about, and so is
//! a target that names more than one of the vault's documents or none of
//! them, which is a fact about what the vault holds rather than about how it
//! is served. What a
//! reload is refused with *before* it runs — a name the registry does not
//! hold, an entry standing on a park, an entry holding nothing to reload yet,
//! an entry whose derived state cannot be trusted — is a fact about the host's
//! serving and stays `host/…`.
//! `engine/…` is a fact about the vault's engine: a rung not enabled, an
//! engine that does not stand, an answer that failed.
//! `request/…` is a fact about the request's own shape, judged apart from
//! any vault: a count it named outside the bound that count holds to, a part
//! it carries that the answer it asks for does not take, or a cursor that
//! names no position among the rows the request pages.
//!
//! A namespace names who the
//! fact is about, never which crate produced it, and a code is *defined*
//! nowhere but here: a layer below stores the code it was handed rather than
//! defining one, and a surface that needs a code it cannot find adds it to a
//! registry rather than spelling a string of its own.
//!
//! **A nested typed reason is structure inside a code, not a code.** The
//! reason a `host/entry-untrusted` detail carries is [`UntrustedReason`], an
//! object whose `kind` tag is a `snake_case` string under the detail's own
//! `reason` key; the [`WatcherLossCause`] inside it is a `snake_case` string
//! that is the value of `cause` rather than a tag. Both are read after the
//! code has been matched, so they carry no namespace, never appear in a code
//! list, and are not values a client dispatches on before it knows the code.
//! Structure grows there without the code list growing. [`WarmingPhase`] sits
//! the same way inside [`TrustState`]: a `snake_case` value under `phase`,
//! read after the `state` tag has been matched.
//!
//! **A note a layer raises about its own reading is not a code.** The text
//! layer files a parse diagnostic under a kebab-case identifier of its own:
//! it names what a reader worked around inside one document, it carries no
//! namespace, and it does not cross this seam. What crosses is what a consumer
//! derives from such notes — a count on a document's row, the typed state that
//! says a document's frontmatter block was read by nothing, or a finding — and
//! a finding is filed under [`FindingKind`] like every other.
//!
//! **A `detail` string is prose and never a match target.** Where a payload
//! carries one — an environmental refusal, a lost watcher — it exists for a
//! person reading a message or a log. Its wording is not contracted, so a
//! client that branches on its text is branching on something free to change;
//! the code and the typed fields beside it are what carry the decision. A
//! detail is diagnostic text for an operator and may name machine-local paths
//! — a store file, a lock file, a vault root — because naming what the machine
//! refused is the point of it. It carries nothing beyond that account of the
//! failure, and it is never content a client parses.
//!
//! # Minting a reason code
//!
//! Each [`ReasonCode`] pairs with exactly one [`ErrorDetail`] variant: the
//! detail's wire tag *is* the code string, and [`ErrorDetail::code`] hands back
//! the code the detail belongs to.

mod address;
mod apply;
mod base64url;
mod cursor;
mod demand;
mod doctor;
mod document;
mod error;
mod finding;
mod finding_row;
mod glob;
mod name;
mod plan;
mod predicate;
mod product;
mod read;
mod reading;
mod reload;
mod status;
mod tag;
mod target;
mod trust;
mod vault;
mod verb;
mod write;

pub use address::{
    Directory, IllegalPath, PollBackend, SchemaSource, UnknownPollBackend, VaultAddress, VaultRoot,
    absolute_path,
};
pub use apply::{
    AppliedTarget, ApplyMode, ApplyParams, ApplyReport, ChangesetOutcome, TargetResult,
};
pub use cursor::{
    Cursor, CursorKey, CursorOrderChanged, FacetKind, HitResume, Moved, NonFiniteScore, OrderPair,
    Page, PagedRows, Score, SidecarRevision, Snapshot,
};
pub use demand::AttachMode;
pub use doctor::{
    DoctorRegistryParams, DoctorRegistryReport, EngineHealth, NoProblems, RegistryProblem,
    RegistrySanity,
};
pub use document::{
    BlockRow, BodyText, Collection, Column, DOCUMENT_EXTENSION, DocumentPath, DocumentRow,
    ElsewhereNamesDocuments, FieldValue, HeadingRow, LinkAddress, LinkAddressKind, LinkFamily,
    LinkHealth, LinkRow, Span, TagRow, TagSource, TotalBelowHead, VAULT_PROTOCOL,
};
pub use error::{
    AnswerShape, ErrorDetail, ErrorEnvelope, MaintainerIdentity, NameSet, ReadFailure, ReasonCode,
    RequestBound, RequestPart, TooFewNames,
};
pub use finding::{FindingKind, FindingScope, Severity, UnknownFindingKind, UnknownSeverity};
pub use finding_row::{CANDIDATE_HEAD, Candidate, CandidateHead, FindingRow, Hint};
pub use glob::{CaseFold, Pattern, PatternError};
pub use name::{IllegalVaultName, VaultName};
pub use plan::backlinks::Backlinks;
pub use plan::document::{
    AuthoredPlan, FileState, LinkKey, OperationsTag, PlanCondition, PlanDocument, Provenance,
    ResolvedPlan, ResolvedTag, Resolves, SkippedFinding, Transition,
};
pub use plan::forecast::{FilePath, FolderPath, Forecast, LinkAdvisory};
pub use plan::hash::{ContentHash, IllegalContentHash};
pub use plan::operation::{
    AuthorCondition, ExpectedField, IllegalOperationId, LinkRewrite, Operation, OperationId,
    OperationKind,
};
pub use plan::outcome::{
    InterruptionCause, PlanFault, RefusedCheck, SchemaViolation, UnresolvedOperation,
    UnresolvedReason,
};
pub use plan::root::RootIdentity;
pub use plan::value::{AuthoredValue, DuplicateKey, FiniteFloat, NonFiniteFloat, ValueMap};
pub use plan::write_target::WriteTarget;
pub use predicate::Predicate;
pub use product::{AnswerAdvisory, ComparedBy, RungSkipReason, Unsatisfied, VaultAnswer};
pub use read::count::{CountParams, CountReport, GroupKey, Tally};
pub use read::describe::{
    ContainerKind, DescribeParams, DescribeReport, Facet, FieldType, PathRuleKind, TagStance,
};
pub use read::find::{Direction, FindParams, FindReport, Sort, SortKey};
pub use read::get::{CollectionPage, CollectionSelector, GetParams, GetReport};
pub use read::search::{
    Hit, NoRetrievalRung, RUNG_DEPTH, RungSelection, RungSet, RungSubtraction, SearchParams,
    SearchReport,
};
pub use read::validate::{KindTally, ValidateParams, ValidateReport};
pub use reading::{
    AnswerReading, EngineSection, Freshness, LadderDeclaration, MalformedLadder, ModelIdentity,
    Rung, RungReport,
};
pub use reload::{ControlFile, ControlFileFailure, ReloadFailure, ReloadStage};
pub use status::{
    Advisory, Attention, Drift, EngineStatus, Fingerprints, Published, Registration, RollUp,
    VaultStatus,
};
pub use tag::fold_tag;
pub use target::{Anchor, IllegalTarget, ResolutionTarget};
pub use trust::{NotReady, TrustState, UntrustedReason, WarmingPhase, WatcherLossCause};
pub use vault::list::{ListParams, ListReport};
pub use vault::register::{RegisterParams, RegisterReport};
pub use vault::reload::{ReloadOutcome, ReloadParams, ReloadReport};
pub use vault::resolve::{ResolveParams, ResolveReport};
pub use vault::set::{VaultChange, VaultReplace, VaultSetParams, VaultSetReport};
pub use vault::status::{StatusParams, StatusReport};
pub use vault::unregister::{UnregisterParams, UnregisterReport};
pub use verb::{
    Addressing, RequestScope, UnknownAddressing, UnknownRequestScope, UnknownVerb, Verb,
};
pub use write::delete::DeleteParams;
pub use write::edit::{DocumentEdit, EditParams};
pub use write::moves::{IllegalMove, MoveParams, MoveSubject};
pub use write::new::NewParams;
pub use write::rewrite_wikilink::RewriteWikilinkParams;
pub use write::set::{FieldChange, SetParams};
