//! The inference firewall, held mechanically: the engine's source names no
//! store surface beyond the feed-read handle (boundary invariant 12).
//!
//! The engine needs a `norn-store` edge to read the feed, so no absent-edge
//! gate can carry the invariant. This test carries the reviewable half: the
//! act invariant 12 refuses is *naming* a wider surface in the crate's
//! source, and a name is a string this gate can read. The suites are outside
//! its subject — they arrange lane-1 state as harness, the same carve-out
//! `norn-testkit` holds.

/// The crate's source, whole. A new module joins this list or the build
/// breaks the include below when the file moves.
const SOURCES: &[(&str, &str)] = &[
    ("src/lib.rs", include_str!("../../src/lib.rs")),
    ("src/ddl.rs", include_str!("../../src/ddl.rs")),
    ("src/engine.rs", include_str!("../../src/engine.rs")),
    ("src/error.rs", include_str!("../../src/error.rs")),
    ("src/sidecar.rs", include_str!("../../src/sidecar.rs")),
];

/// Spellings that reach past the feed-read handle. `Request` covers the
/// write surface's type and every path to it; the verbs are named besides,
/// so a future re-export cannot slip one through under a new type. The
/// `Store` type itself is held by the import gate below, whose parse can
/// tell it from `StoreError`.
const FORBIDDEN: &[&str] = &[
    "Request",
    "begin_request",
    "apply_increment",
    "record_finding",
    "pin_vault_schema",
    "discard_findings",
    "IncrementProvenance",
];

#[test]
fn the_engines_source_names_no_store_surface_beyond_the_feed_read_handle() {
    let mut named = Vec::new();
    for (file, source) in SOURCES {
        for forbidden in FORBIDDEN {
            if source.contains(forbidden) {
                named.push(format!("`{forbidden}` in {file}"));
            }
        }
    }
    assert_eq!(
        named,
        Vec::<String>::new(),
        "the engine reaches past the feed-read handle"
    );
}

/// The whole set of `norn-store` items the source may import. Everything
/// else the engine touches comes back from a feed-read method.
#[test]
fn the_engines_store_imports_are_the_feed_read_set() {
    let allowed = ["DocumentPath", "FeedCursor", "FeedRead", "StoreError"];
    for (file, source) in SOURCES {
        for line in source.lines() {
            let Some(list) = line
                .trim()
                .strip_prefix("use norn_store::")
                .map(|rest| rest.trim_matches(|c| c == '{' || c == '}' || c == ';'))
            else {
                continue;
            };
            for item in list.split(',') {
                let item = item.trim();
                assert!(
                    item.is_empty() || allowed.contains(&item),
                    "{file} imports `{item}` from norn-store, outside the feed-read set"
                );
            }
        }
    }
}
