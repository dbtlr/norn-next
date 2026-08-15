//! The certification machinery: what makes a soak run *qualifying*.
//!
//! Layer 2's exit is not "the suites are green". It is five consecutive
//! qualifying scheduled runs over one frozen candidate, and every word of that
//! needs something checkable behind it. This module holds the three things that
//! carry it, and each answers a question a green check cannot:
//!
//! - [`inventory`] — **which cases the layer requires**, as a table code walks
//!   rather than prose a reader trusts, reconciled in both directions against
//!   what cargo actually compiled. It also carries the committed record of the
//!   trust-transition arms nothing reaches at the production path.
//! - [`manifest`] — **what certified the candidate**: one digest over the lanes,
//!   the toolchain, the resolved dependency graph, the inventory, the rules a
//!   run is judged by, the comparator, the instruments, the fault seams, the
//!   certification suites and the authored bounds. Two runs of a suite that
//!   changed underneath them are two suites, and this is the value that says
//!   so.
//! - [`ledger`] — **what one run was**, as a record a campaign counts rather
//!   than a check somebody remembers: the candidate, the manifest digest, the
//!   platform, the preflight verdict, an outcome per required case, and a
//!   classification with a typed reason where the run does not count, beside
//!   the validator a campaign counts a record through.
//!
//! The campaign that runs five of these over a frozen candidate is not here.
//! What is here is what makes any one of its runs mean something.

pub mod inventory;
pub mod ledger;
pub mod manifest;
