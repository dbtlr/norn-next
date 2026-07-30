//! The serde round trip, over every public type and every variant of each.
//!
//! Two claims, and the second is what makes the first worth anything:
//!
//! 1. **A value survives the wire.** Serializing and deserializing hands back
//!    a value equal to the one that went in.
//! 2. **The bytes are the ones contracted.** A round trip is symmetric even
//!    when both halves are wrong together, so the encoding of each shape is
//!    pinned here as literal JSON.

use norn_wire::{ErrorDetail, ErrorEnvelope, ReasonCode, TrustState, UntrustedReason};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt::Debug;

/// Every reason the vocabulary carries.
fn untrusted_reasons() -> Vec<UntrustedReason> {
    vec![
        UntrustedReason::TornIncrement,
        UntrustedReason::WatcherOverflow,
        UntrustedReason::EnvironmentalRefusal,
    ]
}

/// Every trust state, with one entry per reason behind `Untrusted`.
fn trust_states() -> Vec<TrustState> {
    let mut states = vec![
        TrustState::Unattached,
        TrustState::Warming {
            healed: 0,
            total_estimate: 0,
        },
        TrustState::Warming {
            healed: 12,
            total_estimate: 400,
        },
        TrustState::Ready,
    ];
    states.extend(
        untrusted_reasons()
            .into_iter()
            .map(|reason| TrustState::Untrusted { reason }),
    );
    states
}

/// Every code the list holds.
fn reason_codes() -> Vec<ReasonCode> {
    vec![ReasonCode::HostEntryUntrusted]
}

/// Every detail variant, over every payload it can carry.
fn error_details() -> Vec<ErrorDetail> {
    untrusted_reasons()
        .into_iter()
        .map(|reason| ErrorDetail::EntryUntrusted { reason })
        .collect()
}

fn round_trip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + Debug + PartialEq,
{
    let json = serde_json::to_string(value).expect("serializing a wire type");
    let back: T =
        serde_json::from_str(&json).unwrap_or_else(|error| panic!("reading {json} back: {error}"));
    assert_eq!(&back, value, "the round trip through {json} changed it");
}

fn wire(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("serializing a wire type")
}

// ── The round trip ───────────────────────────────────────────────────────

#[test]
fn every_trust_state_survives_the_round_trip() {
    for state in trust_states() {
        round_trip(&state);
    }
}

#[test]
fn every_untrusted_reason_survives_the_round_trip() {
    for reason in untrusted_reasons() {
        round_trip(&reason);
    }
}

#[test]
fn every_reason_code_survives_the_round_trip() {
    for code in reason_codes() {
        round_trip(&code);
    }
}

#[test]
fn every_error_detail_survives_the_round_trip() {
    for detail in error_details() {
        round_trip(&detail);
    }
}

#[test]
fn every_envelope_survives_the_round_trip() {
    for detail in error_details() {
        round_trip(&ErrorEnvelope::new("the entry is untrusted", detail));
    }
}

/// A message is a person's text, not an identifier: whatever is in it comes
/// back byte for byte.
#[test]
fn a_message_survives_whatever_is_written_in_it() {
    for message in [
        "",
        "the entry is untrusted",
        "quotes \" and \\ and a newline\n",
        "réfusé — 拒否",
    ] {
        round_trip(&ErrorEnvelope::new(
            message,
            ErrorDetail::EntryUntrusted {
                reason: UntrustedReason::TornIncrement,
            },
        ));
    }
}

// ── The bytes ────────────────────────────────────────────────────────────

/// A trust state is an object tagged `state`, and the tag never becomes a key
/// wrapping the payload — that is the external tagging this vocabulary does
/// not use.
#[test]
fn a_trust_state_is_an_object_tagged_state() {
    assert_eq!(wire(&TrustState::Unattached), r#"{"state":"unattached"}"#);
    assert_eq!(wire(&TrustState::Ready), r#"{"state":"ready"}"#);
    assert_eq!(
        wire(&TrustState::Warming {
            healed: 12,
            total_estimate: 400
        }),
        r#"{"state":"warming","healed":12,"total_estimate":400}"#
    );
    assert_eq!(
        wire(&TrustState::Untrusted {
            reason: UntrustedReason::WatcherOverflow
        }),
        r#"{"state":"untrusted","reason":{"kind":"watcher_overflow"}}"#
    );
}

#[test]
fn an_untrusted_reason_is_an_object_tagged_kind() {
    assert_eq!(
        wire(&UntrustedReason::TornIncrement),
        r#"{"kind":"torn_increment"}"#
    );
    assert_eq!(
        wire(&UntrustedReason::WatcherOverflow),
        r#"{"kind":"watcher_overflow"}"#
    );
    assert_eq!(
        wire(&UntrustedReason::EnvironmentalRefusal),
        r#"{"kind":"environmental_refusal"}"#
    );
}

/// The envelope is three fields, and the detail is tagged with the same code
/// the `code` field carries.
#[test]
fn an_envelope_is_a_code_a_message_and_a_detail() {
    let envelope = ErrorEnvelope::new(
        "the entry is untrusted",
        ErrorDetail::EntryUntrusted {
            reason: UntrustedReason::EnvironmentalRefusal,
        },
    );
    assert_eq!(
        wire(&envelope),
        concat!(
            r#"{"code":"host/entry-untrusted","message":"the entry is untrusted","#,
            r#""detail":{"code":"host/entry-untrusted","#,
            r#""reason":{"kind":"environmental_refusal"}}}"#
        )
    );
}

/// The constructor takes the code from the detail, so an envelope cannot be
/// built naming one refusal and describing another.
#[test]
fn an_envelope_takes_its_code_from_its_detail() {
    for detail in error_details() {
        let envelope = ErrorEnvelope::new("refused", detail.clone());
        assert_eq!(envelope.code, detail.code());
        assert_eq!(envelope.detail, detail);
    }
}

/// The reason an entry reports and the reason a refusal carries are one type,
/// so they are one set of bytes too.
#[test]
fn a_refusal_carries_the_same_reason_the_state_does() {
    for reason in untrusted_reasons() {
        let state = TrustState::Untrusted {
            reason: reason.clone(),
        };
        let detail = ErrorDetail::EntryUntrusted {
            reason: reason.clone(),
        };
        let alone = serde_json::to_value(&reason).expect("a reason as JSON");
        let state = serde_json::to_value(&state).expect("a state as JSON");
        let detail = serde_json::to_value(&detail).expect("a detail as JSON");
        assert_eq!(state["reason"], alone);
        assert_eq!(detail["reason"], alone);
    }
}
