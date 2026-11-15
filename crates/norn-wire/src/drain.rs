//! The drain request, v1: how a dialer asks an auto-launched host to retire.
//!
//! `POST /v1/control/drain` with the bearer and the body `{}`; an
//! auto-launched host closes its listener and answers
//! `{ "version": 1, "draining": true }` before it drains. The shape is frozen
//! at first release, so a newer build can retire an older host. A reader
//! ignores a field it does not know.
//!
//! **Dormant carrier.** The serving layer's drain route and the takeover dial
//! read and write these types; neither is built yet, so nothing in the
//! workspace reaches them but their tests.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use crate::object::read_object;

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

    #[test]
    fn a_drain_request_refuses_a_key_given_twice() {
        assert!(serde_json::from_str::<DrainRequest>(r#"{"reason":1,"reason":2}"#).is_err());
    }
}
