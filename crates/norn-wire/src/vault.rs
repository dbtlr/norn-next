//! What each vault-namespace verb is asked for, and what each one answers
//! with.
//!
//! One params type and one report type per verb, in one module per verb, as
//! the read verbs are spelled. A surface renders a verb by rendering two types
//! rather than by assembling a request out of parts it chose.
//!
//! **A registry verb carries no vault address.** Registering, unregistering,
//! listing, editing and resolving are answered from the serving set and the
//! registry as a whole, so their params name what they act on — a name, a
//! registration, a directory — and nothing addresses a vault to be held.
//! [`Verb::addressing`](crate::Verb::addressing) says the same thing, and the
//! verb table holds the two together.
//!
//! **`vault status` and `vault reload` carry a vault address.** Both are
//! vault-scope requests, and every vault-scope params type in the vocabulary
//! names its vault by a [`VaultAddress`](crate::VaultAddress) —
//! [`Addressing`](crate::Addressing) says so for every verb that carries one.
//! An address naming a root asks for a throwaway attach, which has no
//! lifecycle to observe and nothing to reload, so the host refuses it
//! `host/unsupported-attach-mode` here as it does for every read.
//!
//! **Neither of them carries an answer reading.** A status and a reload are
//! lifecycle observations rather than reads: a status is taken off what an
//! entry already publishes and creates no demand, and a reload reports what it
//! applied. Neither answers from a database, so neither has an epoch or a
//! generation to have been answered under, and their reports cross as
//! themselves rather than inside a
//! [`VaultAnswer`](crate::VaultAnswer). Registry reports cross the same way,
//! for the same reason.
//!
//! **A refusal of the request is never one of its report's shapes.** Each
//! verb's refusals are named in its own module documentation as the codes a
//! client meets, and each of them is an
//! [`ErrorEnvelope`](crate::ErrorEnvelope) beside the report rather than a
//! variant inside it. A report may still *carry* a refusal another surface
//! published, as a parked entry's status does: the park's own envelope is
//! what that entry publishes, and reporting it is an answer rather than a
//! refusal of the status request.

pub(crate) mod list;
pub(crate) mod register;
pub(crate) mod reload;
pub(crate) mod resolve;
pub(crate) mod set;
pub(crate) mod status;
pub(crate) mod unregister;
