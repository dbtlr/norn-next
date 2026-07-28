//! The composition root: the one shipped artifact.
//!
//! Its whole job is to wire the serving side and the client side together —
//! see the crate map in `docs/architecture.md`. Neither exists yet, so this
//! binary does nothing. It exists now because the argv corpus lives in this
//! package, which is the one place cargo makes the built binary reachable
//! from an integration test.

fn main() {}
