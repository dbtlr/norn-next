//! The native wire route: wire params in, wire reports out, dispatched to
//! `norn-host`.
//!
//! **Reserved.** The route's dispatch lands here in a later task, under the
//! name `dispatch`. The file exists before it does because the regression
//! registry grounds every case the served request path will observe on that
//! symbol's absence from this file, and an absence is claimed only in a file
//! the tree holds; the day `dispatch` is declared here, each of those cases
//! fails its audit until it is bound or re-stated against the next surface
//! that observes it.
