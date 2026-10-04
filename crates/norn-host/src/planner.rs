//! The one planner: a plan's operations in, a resolved plan and its forecast
//! out, or the fault in the plan's own shape.
//!
//! **What the planner owns.** Invariant 4 names one plan vocabulary and one
//! applier; this is the one path from operations to the transitions that
//! applier executes. A write verb's operations, a caller's authored
//! plan, and the operations a refused apply re-resolves for its fresh plan all
//! plan here, through [`resolve::resolve`], so an operation means the same
//! thing wherever it arrives from. A refresh names the operations an earlier
//! apply already landed, which it drops, so a remaining operation's
//! requirement on one of them is met rather than a fault. Planning is seven
//! steps, each its own module, expansion two:
//!
//! - [`expand`] — each `where` target turned into one operation per document
//!   it matches, each naming its document by path, through the find builder
//!   on the one snapshot the request holds, and each folder move into one
//!   document move per document the folder holds, naming every file it
//!   leaves behind; a `where` matching nothing, or nothing as asked, and a
//!   folder move with nothing to move are left unresolved in words; then
//!   [`rule`] turns each creation by rule into the one `create_document` its
//!   rule makes, at its place, its path filled and numbered and its content
//!   composed from the pinned schema's rule and one clock reading for the
//!   plan, or leaves it unresolved naming why. What follows plans only path
//!   targets and concrete creates.
//! - [`order`] — the plan's shape: the order its operations compose in, and
//!   the faults `request/plan-invalid` answers — an identifier carried twice,
//!   a requirement nothing carries, a cycle of requirements, and a cycle of
//!   content, such as two documents exchanging places. A move or a create
//!   waits for what vacates its name only where a document other than its
//!   own source stands there at planning, so the vault decides whether a
//!   chain of moves is a cycle.
//! - [`compose`] — each file's after-bytes from its before-bytes and the
//!   operations touching it, in that order. An operation that cannot act on
//!   what it meets — an edit whose text does not occur exactly once, a create
//!   or a move onto a document, onto a folder or beneath a document, or any
//!   operation naming a file whose normalized spelling the store's path
//!   grammar refuses — does not resolve; it is not a fault in the plan. A
//!   document-local kind — a frontmatter field set, removed, pushed to or
//!   popped from, a body or a section replaced, text appended or inserted at a
//!   heading — edits its document where it stands through [`edit`], which
//!   also names what does not resolve: a push onto a scalar, a pop or a
//!   remove of what the document does not hold, a heading that is ambiguous,
//!   missing or inside a container, empty content to append or insert, and
//!   a `where` target [`expand`] did not expand. The pop,
//!   the remove and the empty content are left unresolved where they would
//!   otherwise land as a silent no-op. An edit that rewrites what its
//!   document already holds — a field set to the value it holds, a body or a
//!   section replaced by itself — resolves to a transition whose after-state
//!   is its before-state, which lands found (ADR 0032's landed rule).
//! - [`cascade`] — each link cascade: once every operation acts, the links
//!   that would stop naming a moved document, that name a document a delete
//!   removes rewriting them, or that name a wikilink rewrite's `old`, read
//!   through the store's resolution door as the change set reads them, each
//!   respelled in its own style to the shortest spelling that reads back as
//!   the document it must name, and carried on the operation. A cascade's
//!   holder is a file its operation touches, so the two stand or fall
//!   together.
//! - [`resolve`] — the resolved plan: an operation that does not resolve, or
//!   whose author's condition the vault no longer meets or names a file the
//!   store's path grammar refuses, is left out, with
//!   every operation that touches one of its files or requires it, and what
//!   remains is one transition per file, the author conditions on files the
//!   plan does not write, the root's identity and the plan's force. An
//!   author's condition — a content hash or an expected frontmatter value —
//!   is judged against the before-state of a file the plan writes, which it
//!   then says nothing beyond, and against any other file as it stands, which
//!   travels as a condition on that file's content.
//! - [`lineage`] — the content cycle the order cannot see: where the resolved
//!   operations' targets draw on each other's before-states through a name
//!   nothing stood at, such as two documents exchanging places through a
//!   temporary name, the plan is refused as `request/plan-invalid`, as a
//!   direct exchange is by [`order`]. The same derivation is the applier's:
//!   it follows a resolved plan's content through its moves in the recorded
//!   order, and refuses its content cycles by this one rule.
//! - [`forecast`](mod@forecast) — the folders the plan makes and removes.
//!
//! **Composition is shared with the applier.** A target's after-bytes are a
//! pure function of the before-bytes of the files the plan touches and the
//! operations, taken in the order a resolved plan carries them, which is the
//! one [`order::dependencies`] settled. The planner hashes that result into
//! each transition; the applier recomposes with [`compose::compose`] and
//! refuses unless the result hashes to the after-state the plan carries.
//!
//! **A control file is planned, never as a document.** A
//! `write_control_file` names the vault schema or the vault config by role,
//! and [`control`] maps the role to the path a transition names it at: the
//! in-vault path the host reads it at, which for the schema stands for the
//! file the registration reads it from, a `schema_source` included (ADR
//! 0034). The target is read through
//! [`VaultView::control_entry`](view::VaultView::control_entry) — the vault's
//! walk does not enter the schema's path, so reading it as a document would
//! find a place no document can be — and composed as the whole content the
//! operation carries, created where it is absent and replaced where it
//! stands, under the before-state it held. Content its role's parser refuses
//! does not resolve. A control file is no link's candidate and holds no link
//! the vault reads, and a plan writing one beside a document operation is a
//! fault in its shape (ADR 0032: a plan that changes a vault control file
//! changes nothing else).
//!
//! **A file is its identity.** Every name an operation carries is read
//! through `norn-fs`'s one path-spelling normalization point, which the
//! [`view::VaultView`] exposes under the case behavior the root proved: two
//! spellings of one file compose as one target, held at the spelling the
//! tree lists. On a root that folds case a destination differing from its
//! source only in case names the source itself (ADR 0032), so a case-only
//! rename plans as a move — the old spelling from present to absent and the
//! new one from absent to present — which the applier publishes as
//! `norn-fs`'s respell. The name it arrives at is the one it vacates, so it
//! waits for no other operation vacating that name, and an operation after it
//! in the plan — a removal, a move on, a rename back — acts on the document at
//! its new spelling. A folder's change of case is not planned: an operation
//! naming a folder in a case the tree does not list, or in a case other than
//! the one an earlier operation of the plan made it in, is left unresolved.
//!
//! **Before-states are read from the files, not the store.** A before-state is
//! the hash of the bytes a target is composed from, so both sides read them
//! through a [`view::VaultView`]: the store holds no document's exact bytes,
//! and a hash read from it could name bytes nobody composed against. The
//! request's one snapshot is what an operation that reads derived facts plans
//! against, and two do: a `where` target, which [`expand`] matches there, and
//! a carried move's own links, which [`links::vouched`] reads there in place
//! of the body the move never holds. Each is held to the files: the match
//! names documents, the files then say what each holds, and a matched
//! document the files no longer hold does not resolve; the index's links of
//! a carried document are taken only where the hash it derived them from is
//! the hash streamed from the file, and read from the file itself otherwise
//! ([`resolve`]), so no plan rests on a fact the files contradict.
//!
//! **A create publishes before any removal.** ADR 0032 publishes creates
//! first and removals last, so a name a removal of this plan vacates still
//! stands when a create publishes: a create beneath a document the plan
//! removes, or at a folder the plan empties, does not resolve, while a move
//! or create onto a document another operation moves away or removes runs
//! after it and resolves.
//!
//! **What the planner leaves to the applier.** Whether a composed result
//! passes the vault schema, and whether a forced plan lets a violation
//! through, is checked while every target is staged, as the architecture's
//! apply seam places it, and a preview answers from the applier's own
//! judgment of the plan it resolves.
//! Publishing a case-only rename's two transitions as one respell is the
//! applier's, as is checking the plan's conditions again, landed or not.
//!
//! **Memory.** A resolved plan carries its operations and one fixed-size
//! transition per target, never the bytes of a file it did not author.
//! Planning itself holds the bytes of every file the plan touches until it
//! answers, but for a document a move carries byte for byte by the one rule
//! planning and the applier share ([`lineage::Carried`]): that one is read
//! streamed and carried unread ([`compose::After::Carried`]), its links taken
//! from the store's index where the index derived them from those very bytes
//! ([`links::vouch_for_carried`]). **No copy once indexed**: where the index
//! does not vouch for them, planning reads the file whole once at the hash
//! it streamed and takes its links from its bytes — one copy, as it read
//! every moved document before it carried any — so a move over a vault whose
//! index lags its files plans as it always did. A moved document an edit, a
//! link rewrite or a cascade lands on is no carried one: it is read whole,
//! once, held once and parsed once; where its own cascade lands, an earlier
//! pass streamed it, so the file is opened twice — I/O, not memory.
//!
//! **Who plans here.** The applier ([`crate::applier`]) recomposes every
//! target through [`compose::compose`] and re-resolves a refused plan's
//! operations through [`resolve::resolve`] for refuse-and-refresh — a
//! resolved plan's operations carry only path targets, so nothing is matched
//! again; the apply job plans an authored plan through
//! [`expand::resolve_expanding`] inside the entry's claim over a
//! [`view::TreeView`], matching on a snapshot it takes there; and a preview
//! plans the same way on its one snapshot, taking no claim (`crate::apply`).

pub(crate) mod cascade;
pub(crate) mod compose;
pub(crate) mod control;
pub(crate) mod edit;
pub(crate) mod expand;
pub(crate) mod forecast;
pub(crate) mod lineage;
pub(crate) mod links;
pub(crate) mod order;
pub(crate) mod resolve;
pub(crate) mod rule;
pub(crate) mod view;
