//! What each read verb is asked for, and what each one answers with.
//!
//! One params type and one report type per verb, in one module per verb. A
//! verb's params are the whole of what a request carries and its report is the
//! whole of what an answer holds, so a surface renders a verb by rendering two
//! types rather than by assembling a request out of parts it chose.
//!
//! **Every read answers `Result<VaultAnswer<Report>, ErrorEnvelope>`.** The
//! three exits are [`product`](crate::product)'s, and nothing here repeats
//! them: a report is only ever the third field of an answer, and a refusal is
//! never one of a report's shapes.
//!
//! **Params carry the vault first and nothing optional in the
//! constructor.** Each type's `new` takes the fields a request cannot be built
//! without — the vault address, and the query or target where the verb has one
//! — and every other field has a stated default reached through a `with_`
//! setter. A request built by naming three things is a request whose other
//! seven parts read the same way at every surface, rather than seven arguments
//! a caller spells `None` for.
//!
//! **A verb's shape is what the verb answers, not what its neighbours
//! answer.** `find` is the sole verb whose rows *are* documents: they are
//! ordered by a field and paged by `(sort value, path)`. Three verbs carry a
//! projected document row without being that verb — a `search` hit and a `get`
//! record carry a document's projected columns beside their own identity, a
//! score or a target, and are ordered by their own key. `search` carries no
//! sort, because a ranked answer is ordered by its ranking; `count` carries no
//! columns, because a tally is not a document; `validate` runs no rule at
//! request time and emits no plan. Each params type says so where a reader of
//! that verb will look, rather than leaving the partition to be inferred from
//! six types side by side.
//!
//! **Links are never a verb.** A link is reached three ways and each of them
//! is a shape that already exists: `links_to` is a
//! [`Predicate`](crate::Predicate), links are a [`Column`](crate::Column) on a
//! document row, and link health is a finding family. A seventh verb for them
//! would be a fourth way to ask one question.
//!
//! **`limit` bounds the rows a page carries and nothing else.** Whichever row
//! a verb pages — a document, a hit, a tally, a finding, a facet, or a row of
//! the one nested collection a `get` pages — the limit says how
//! many of them come back. It never says how much of one row does: a nested
//! collection carried *on* a row is bounded by the per-row ceiling the handler
//! keeps and reports what it cut through
//! [`Collection::total`](crate::Collection::total), and a body carried on a
//! row reports its cut through
//! [`BodyText::byte_length`](crate::BodyText::byte_length) the same way.

pub(crate) mod count;
pub(crate) mod describe;
pub(crate) mod find;
pub(crate) mod get;
pub(crate) mod search;
pub(crate) mod validate;
