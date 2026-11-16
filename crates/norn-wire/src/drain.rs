//! The drain request, v1: how a dialer asks an auto-launched host to retire.
//!
//! `POST /v1/control/drain` with the bearer and the body `{}`; an
//! auto-launched host closes its listener and answers
//! `{ "version": 1, "draining": true }` before it drains. The shape is frozen
//! at first release, so a newer build can retire an older host. A reader
//! ignores a field it does not know.
//!
//! The serving layer's drain route writes these types, and its takeover dial
//! reads a reply by [`DrainReply::read`], so a newer build's dialer reads an
//! older host's answer through one spelling.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::error::{ErrorEnvelope, ReasonCode};
use crate::object::read_object;

/// A reply to the drain request, read from its status and body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrainReply {
    /// A 202 whose body is a drain answer: the host set `draining` and closed
    /// its listener before it answered.
    Draining,
    /// A 503 `serve/draining`: the host had already begun to drain.
    AlreadyDraining,
    /// A 409 `serve/drain-refused`: the host was started explicitly and keeps
    /// serving.
    Refused,
    /// Anything else — a 401 after the token set changed between the hello
    /// and the drain, a 500 `serve/token-file-unreadable`, or a body no host
    /// answers with — which says nothing verified about the host.
    Unrecognized,
}

impl DrainReply {
    /// Read a reply from its HTTP status code and its body.
    pub fn read(status: u16, body: &[u8]) -> Self {
        match status {
            202 => match serde_json::from_slice::<DrainAnswer>(body) {
                Ok(answer) if answer.is_draining() => DrainReply::Draining,
                _ => DrainReply::Unrecognized,
            },
            409 | 503 => match serde_json::from_slice::<ErrorEnvelope>(body) {
                Ok(envelope) => match (status, envelope.code()) {
                    (409, ReasonCode::ServeDrainRefused) => DrainReply::Refused,
                    (503, ReasonCode::ServeDraining) => DrainReply::AlreadyDraining,
                    _ => DrainReply::Unrecognized,
                },
                Err(_) => DrainReply::Unrecognized,
            },
            _ => DrainReply::Unrecognized,
        }
    }
}

/// The drain request's body: `{}`.
///
/// Read as an object and nothing else. A derived reader of an empty struct
/// also takes `[]`, which is not the shape; the reader here takes any object,
/// each key once, and ignores what it holds.
#[derive(Clone, Copy, Debug, Default, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DrainRequest {}

impl<'de> Deserialize<'de> for DrainRequest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        read_object(deserializer)?;
        Ok(DrainRequest {})
    }
}

impl DrainRequest {
    pub const fn new() -> Self {
        DrainRequest {}
    }
}

/// The answer a host that has begun to drain gives:
/// `{ "version": 1, "draining": true }`.
///
/// Read as an object and nothing else, each key once, ignoring a field it does
/// not know: a derived reader would also take the two values as an array.
#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize)]
pub struct DrainAnswer {
    /// The drain shape's version: 1.
    version: u32,
    /// Always `true`: a host that is not draining answers with a refusal.
    draining: bool,
}

impl<'de> Deserialize<'de> for DrainAnswer {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// The answer's fields, read out of an object [`read_object`] took.
        #[derive(Deserialize)]
        struct Fields {
            version: u32,
            draining: bool,
        }
        let object = read_object(deserializer)?;
        let Fields { version, draining } =
            Fields::deserialize(serde_json::Value::Object(object)).map_err(D::Error::custom)?;
        Ok(DrainAnswer { version, draining })
    }
}

impl DrainAnswer {
    /// The version this shape is.
    pub const VERSION: u32 = 1;

    /// The answer a host gives once it has closed its listener.
    pub const fn draining() -> Self {
        DrainAnswer {
            version: DrainAnswer::VERSION,
            draining: true,
        }
    }

    pub const fn version(self) -> u32 {
        self.version
    }

    pub const fn is_draining(self) -> bool {
        self.draining
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drain_request_is_an_empty_object_and_ignores_what_it_does_not_know() {
        assert_eq!(
            serde_json::to_string(&DrainRequest::new()).expect("a request as JSON"),
            "{}"
        );
        assert_eq!(
            serde_json::from_str::<DrainRequest>(r#"{"reason":"update"}"#)
                .expect("a request with a field this build does not know"),
            DrainRequest::new()
        );
        for not_an_object in ["[]", "null", "\"{}\""] {
            assert!(
                serde_json::from_str::<DrainRequest>(not_an_object).is_err(),
                "`{not_an_object}` read as a drain request"
            );
        }
    }

    #[test]
    fn a_drain_answer_is_written_as_the_contract_spells_it() {
        let answer = DrainAnswer::draining();
        let json = serde_json::to_string(&answer).expect("an answer as JSON");
        assert_eq!(json, r#"{"version":1,"draining":true}"#);
        let read: DrainAnswer = serde_json::from_str(r#"{"version":1,"draining":true,"since":3}"#)
            .expect("an answer with a field this build does not know");
        assert_eq!(read, answer);
        assert_eq!(read.version(), 1);
        assert!(read.is_draining());
    }

    /// An answer is an object, as the request is: a derived reader would also
    /// take its two values as an array, which is not the shape. A key given
    /// twice is refused rather than read as whichever came last.
    #[test]
    fn a_drain_answer_is_read_from_an_object_only() {
        for not_an_answer in [
            "[1, true]",
            "null",
            r#"{"version":1,"draining":true,"draining":false}"#,
            r#"{"version":1}"#,
        ] {
            assert!(
                serde_json::from_str::<DrainAnswer>(not_an_answer).is_err(),
                "`{not_an_answer}` read as a drain answer"
            );
        }
    }

    /// Only a 503 whose code is `serve/draining` reads as draining, and only
    /// a 409 whose code is `serve/drain-refused` as a refusal: any other 503
    /// or 409, and every other status, is no verified answer.
    #[test]
    fn a_drain_reply_is_read_by_its_status_and_its_code() {
        use crate::error::ErrorDetail;
        let envelope = |detail| {
            serde_json::to_vec(&ErrorEnvelope::new("refused", detail)).expect("an envelope")
        };
        let answer = serde_json::to_vec(&DrainAnswer::draining()).expect("an answer");
        let cases: [(u16, Vec<u8>, DrainReply); 8] = [
            (202, answer.clone(), DrainReply::Draining),
            (
                202,
                br#"{"version":1,"draining":false}"#.to_vec(),
                DrainReply::Unrecognized,
            ),
            (200, answer, DrainReply::Unrecognized),
            (
                503,
                envelope(ErrorDetail::draining()),
                DrainReply::AlreadyDraining,
            ),
            (503, envelope(ErrorDetail::busy()), DrainReply::Unrecognized),
            (
                409,
                envelope(ErrorDetail::drain_refused()),
                DrainReply::Refused,
            ),
            (
                401,
                envelope(ErrorDetail::unauthorized()),
                DrainReply::Unrecognized,
            ),
            (
                500,
                envelope(ErrorDetail::token_file_unreadable()),
                DrainReply::Unrecognized,
            ),
        ];
        for (status, body, expected) in cases {
            assert_eq!(DrainReply::read(status, &body), expected, "{status}");
        }
    }

    #[test]
    fn a_drain_request_refuses_a_key_given_twice() {
        assert!(serde_json::from_str::<DrainRequest>(r#"{"reason":1,"reason":2}"#).is_err());
    }
}
