//! The reason codes, string by string.
//!
//! A code's flat namespaced spelling is contract: it is what a caller matches
//! on, what a log carries, and what a schema advertises. It is written out
//! here so that a rename is a deliberate edit to this table rather than a
//! side effect of renaming a Rust variant.

use norn_wire::{ErrorDetail, ReasonCode, UntrustedReason};

/// Every code the list holds, paired with the exact string it is on the wire.
const CODES: &[(ReasonCode, &str)] = &[(ReasonCode::HostEntryUntrusted, "host/entry-untrusted")];

#[test]
fn every_code_serializes_to_its_flat_namespaced_string() {
    for (code, string) in CODES {
        assert_eq!(
            serde_json::to_string(code).expect("serializing a code"),
            format!("\"{string}\""),
        );
    }
}

#[test]
fn every_code_reads_back_from_its_flat_namespaced_string() {
    for (code, string) in CODES {
        let back: ReasonCode = serde_json::from_str(&format!("\"{string}\""))
            .unwrap_or_else(|error| panic!("reading {string} back: {error}"));
        assert_eq!(back, *code);
    }
}

/// The Rust-side spelling and the wire-side one are two statements of the same
/// string, and this is what keeps them one.
#[test]
fn every_code_names_itself_the_same_way_in_rust() {
    for (code, string) in CODES {
        assert_eq!(code.as_str(), *string);
    }
}

/// The shape of a code, not just its spelling: one namespace, one slash, and
/// lowercase kebab-case on both sides of it.
#[test]
fn every_code_is_one_namespace_and_one_kebab_case_name() {
    for (_, string) in CODES {
        let (namespace, name) = string
            .split_once('/')
            .unwrap_or_else(|| panic!("`{string}` carries no namespace"));
        assert!(
            !name.contains('/'),
            "`{string}` carries more than one separator"
        );
        for segment in [namespace, name] {
            assert!(!segment.is_empty(), "`{string}` has an empty segment");
            assert!(
                segment
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "`{string}` is not lowercase kebab-case"
            );
            assert!(
                !segment.starts_with('-') && !segment.ends_with('-'),
                "`{string}` has a segment bounded by a hyphen"
            );
        }
    }
}

/// A detail is tagged with the code it belongs to, so the code a detail
/// carries on the wire and the code the enum reports are one string.
#[test]
fn every_detail_is_tagged_with_the_code_it_belongs_to() {
    let details = [
        UntrustedReason::TornIncrement,
        UntrustedReason::WatcherOverflow,
        UntrustedReason::EnvironmentalRefusal,
    ]
    .map(|reason| ErrorDetail::EntryUntrusted { reason });
    for detail in details {
        let json = serde_json::to_value(&detail).expect("a detail as JSON");
        assert_eq!(json["code"].as_str(), Some(detail.code().as_str()));
    }
}
