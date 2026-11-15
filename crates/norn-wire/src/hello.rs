//! The hello, v1: the one exchange a host answers without a bearer, and the
//! bytes its MAC is computed over.
//!
//! A dialer sends the label of the token it holds and a random nonce; the host
//! answers with the facts a dialer checks — its channel, build, launch kind
//! and serving state, and both ends of this connection as its socket reports
//! them — and an HMAC-SHA256 over those facts and the nonce, keyed by the
//! labelled token's secret. The dialer verifies the MAC under the token it
//! read before it sends that token as a bearer on the connection.
//!
//! **One spelling for both halves.** The host and every dialer build the MAC
//! input through [`HelloFacts::mac_input`], from the same typed facts and the
//! same nonce, so the two halves cannot drift: there is no second place the
//! bytes are written. The HMAC itself is computed by the serving and client
//! crates; this vocabulary takes no cryptographic dependency.
//!
//! **The shape is frozen at first release.** A newer build keeps answering
//! this shape at `/v1/hello` for as long as any supported release can dial it.
//! A v1 reader ignores a field it does not know, so a field may be added to
//! the answer — but never to the MAC input, which would make every older
//! dialer's check fail.
//!
//! **The dialer's check is spelled here too**, crypto-free: what a reply's
//! status and body say ([`HelloReply::read`]), whether an address may be
//! dialed at all ([`is_dialable`]), how large a hello body may be
//! ([`HELLO_BODY_BYTES`]), how a MAC-verified answer's facts are compared
//! with the dialer's connection ([`HelloFacts::check_connection`]), and how a
//! failed dial is reported ([`HelloFailure`]). The two dialers —
//! `norn-client`'s and the serving crate's takeover dial — keep only their
//! transport and their HMAC check, so neither their comparisons nor their
//! reports can drift.
//!
//! The serving layer's hello route reads and writes these types, and so do
//! both dialers.

use std::borrow::Cow;
use std::fmt;
use std::net::{SocketAddr, SocketAddrV4, SocketAddrV6};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

use crate::build::BuildIdentity;
use crate::error::{ErrorEnvelope, ReasonCode};
use crate::hex;
use crate::object::read_object;

/// The most bytes a hello body may be, in either direction: the host refuses
/// a larger request with `serve/request-too-large` before reading it whole,
/// and a dialer reads no more of an answer than this before parsing it. The
/// MAC input's length prefixes are 4 bytes wide, so this is also what keeps a
/// hostile listener's answer far from the field length they cannot carry.
pub const HELLO_BODY_BYTES: usize = 4096;

/// The ASCII domain separator the MAC input opens with, before its zero byte.
const MAC_DOMAIN: &[u8] = b"norn-hello-v1";

/// The 32 random bytes a dialer sends with its hello, written on the wire as
/// 64 lowercase hexadecimal digits.
///
/// The host answers a nonce it cannot read with `serve/hello-refused`, the
/// refusal an unknown label earns, rather than a malformed-request refusal:
/// which of the two was wrong is nothing a caller nobody has verified needs to
/// learn. So a [`HelloRequest`] carries the nonce as text, and it is read
/// through [`Nonce::from_hex`] after the body has parsed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Nonce([u8; Nonce::BYTES]);

impl Nonce {
    /// How many bytes a nonce is.
    pub const BYTES: usize = 32;

    /// The nonce `bytes` are — the dialer's, drawn from its random source.
    pub const fn from_bytes(bytes: [u8; Nonce::BYTES]) -> Self {
        Nonce(bytes)
    }

    /// The nonce `text` spells, or `None` unless it is exactly 64 lowercase
    /// hexadecimal digits.
    pub fn from_hex(text: &str) -> Option<Self> {
        hex::decode_array(text).map(Nonce)
    }

    /// The nonce as the wire writes it: 64 lowercase hexadecimal digits.
    pub fn to_hex(&self) -> String {
        hex::encode(&self.0)
    }

    pub const fn as_bytes(&self) -> &[u8; Nonce::BYTES] {
        &self.0
    }
}

/// The hello's request body: `{ "label": "...", "nonce": "<64 hex>" }`.
///
/// Both fields are held as the text that arrived. A body that is not JSON of
/// this shape at all is a malformed request; a body that is, but whose label
/// names no stored token or whose nonce does not read, is a refused hello.
/// Keeping the two as text is what lets the host tell those apart. The label's
/// grammar is the token store's, and the nonce's is [`Nonce::from_hex`].
///
/// Read as an object and nothing else, each key once, ignoring a field it
/// does not know: a derived reader would also take the two values as an
/// array, which is not the shape.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
pub struct HelloRequest {
    /// The label of the token the dialer holds, which keys the answer's MAC.
    label: String,
    /// The dialer's nonce, as 64 lowercase hexadecimal digits.
    nonce: String,
}

impl<'de> Deserialize<'de> for HelloRequest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let object = read_object(deserializer)?;
        let text = |key: &'static str| match object.get(key) {
            Some(serde_json::Value::String(text)) => Ok(text.clone()),
            Some(_) => Err(D::Error::custom(format!("`{key}` is not a string"))),
            None => Err(D::Error::missing_field(key)),
        };
        Ok(HelloRequest {
            label: text("label")?,
            nonce: text("nonce")?,
        })
    }
}

impl HelloRequest {
    /// The request a dialer sends: the label of its token and its nonce.
    pub fn new(label: impl Into<String>, nonce: &Nonce) -> Self {
        HelloRequest {
            label: label.into(),
            nonce: nonce.to_hex(),
        }
    }

    /// The label as it arrived, for the token store's grammar to read.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The nonce, or `None` when what arrived is not 64 lowercase
    /// hexadecimal digits.
    pub fn nonce(&self) -> Option<Nonce> {
        Nonce::from_hex(&self.nonce)
    }
}

/// Which channel the answering host serves: `dev` or `live`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HelloChannel {
    Dev,
    Live,
}

impl HelloChannel {
    /// The wire spelling, which is also what the MAC input holds.
    pub const fn as_str(self) -> &'static str {
        match self {
            HelloChannel::Dev => "dev",
            HelloChannel::Live => "live",
        }
    }
}

/// How the answering host was started: `explicit` (by hand or by a service
/// manager) or `auto` (by a client's auto-launch).
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HelloLaunch {
    Explicit,
    Auto,
}

impl HelloLaunch {
    /// The wire spelling, which is also what the MAC input holds.
    pub const fn as_str(self) -> &'static str {
        match self {
            HelloLaunch::Explicit => "explicit",
            HelloLaunch::Auto => "auto",
        }
    }
}

/// Whether the answering host is `serving` or has begun to drain.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HelloState {
    Serving,
    Draining,
}

impl HelloState {
    /// The wire spelling, which is also what the MAC input holds.
    pub const fn as_str(self) -> &'static str {
        match self {
            HelloState::Serving => "serving",
            HelloState::Draining => "draining",
        }
    }
}

/// A socket address as the hello compares and writes it: an IPv4-mapped IPv6
/// address as the IPv4 address it maps, and an IPv6 address with no flow
/// label and no scope.
///
/// The host reports both ends of a connection as its socket reports them, and
/// a dual-stack socket can report an IPv4 peer in its mapped form; the dialer
/// compares them with the address it dialed and its own local address. Both
/// halves pass every address through this, so the two compare one spelling.
pub fn normalize_socket_address(address: SocketAddr) -> SocketAddr {
    match address {
        SocketAddr::V4(_) => address,
        SocketAddr::V6(v6) => match v6.ip().to_ipv4_mapped() {
            Some(v4) => SocketAddr::V4(SocketAddrV4::new(v4, v6.port())),
            None => SocketAddr::V6(SocketAddrV6::new(*v6.ip(), v6.port(), 0, 0)),
        },
    }
}

/// A [`normalize_socket_address`]-ed address, written `ip:port` (an IPv6
/// address as `[ip]:port`) on the wire and in the MAC input.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct WireAddress(SocketAddr);

impl WireAddress {
    fn new(address: SocketAddr) -> Self {
        WireAddress(normalize_socket_address(address))
    }
}

impl fmt::Display for WireAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A normalized V6 address carries no scope, so this is `[ip]:port`.
        self.0.fmt(f)
    }
}

impl Serialize for WireAddress {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for WireAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = Cow::<str>::deserialize(deserializer)?;
        text.parse::<SocketAddr>()
            .map(WireAddress::new)
            .map_err(|_| D::Error::custom(format!("`{text}` is not an `ip:port` socket address")))
    }
}

impl JsonSchema for WireAddress {
    fn schema_name() -> Cow<'static, str> {
        "SocketAddress".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "A socket address, written `ip:port` (an IPv6 address as `[ip]:port`), an IPv4-mapped IPv6 address written as the IPv4 address it maps."
        })
    }
}

/// The facts a hello answer states, which are every field its MAC covers.
///
/// `version` and `mac` are the answer's own and are not here: the version is
/// the route's, and the MAC cannot cover itself.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct HelloFacts {
    /// The channel the host serves.
    channel: HelloChannel,
    /// The host's build identity.
    build: String,
    /// How the host was started.
    launch: HelloLaunch,
    /// Whether it is serving or draining.
    state: HelloState,
    /// This connection's local end, as the host's socket reports it.
    host: WireAddress,
    /// This connection's peer end — the dialer — as the host's socket reports
    /// it.
    client: WireAddress,
}

impl HelloFacts {
    /// The facts a host states about itself and one connection. Both
    /// addresses are normalized here, so a value of this type holds only the
    /// spelling the wire writes.
    pub fn new(
        channel: HelloChannel,
        build: BuildIdentity,
        launch: HelloLaunch,
        state: HelloState,
        host: SocketAddr,
        client: SocketAddr,
    ) -> Self {
        HelloFacts {
            channel,
            build: build.as_str().to_string(),
            launch,
            state,
            host: WireAddress::new(host),
            client: WireAddress::new(client),
        }
    }

    pub const fn channel(&self) -> HelloChannel {
        self.channel
    }

    /// The host's build identity as it was written.
    pub fn build(&self) -> &str {
        &self.build
    }

    /// Whether the host is `build`: the same build exactly when the strings
    /// are equal.
    pub fn is_build(&self, build: BuildIdentity) -> bool {
        self.build == build.as_str()
    }

    pub const fn launch(&self) -> HelloLaunch {
        self.launch
    }

    pub const fn state(&self) -> HelloState {
        self.state
    }

    /// The connection's host end, normalized.
    pub const fn host(&self) -> SocketAddr {
        self.host.0
    }

    /// The connection's client end, normalized.
    pub const fn client(&self) -> SocketAddr {
        self.client.0
    }

    /// The bytes the hello's MAC is computed over, for `nonce`.
    ///
    /// The ASCII `norn-hello-v1`, one zero byte, then the nonce's 32 raw
    /// bytes, `channel`, `build`, `launch`, `state`, `host` and `client`, in
    /// that order, each preceded by its length as a 4-byte big-endian
    /// integer. Each string field is its wire spelling. The length prefixes
    /// make the encoding unambiguous: no two sets of fields produce the same
    /// bytes.
    pub fn mac_input(&self, nonce: &Nonce) -> Vec<u8> {
        let host = self.host.to_string();
        let client = self.client.to_string();
        let fields: [&[u8]; 7] = [
            nonce.as_bytes(),
            self.channel.as_str().as_bytes(),
            self.build.as_bytes(),
            self.launch.as_str().as_bytes(),
            self.state.as_str().as_bytes(),
            host.as_bytes(),
            client.as_bytes(),
        ];
        let length =
            MAC_DOMAIN.len() + 1 + fields.iter().map(|field| 4 + field.len()).sum::<usize>();
        let mut input = Vec::with_capacity(length);
        input.extend_from_slice(MAC_DOMAIN);
        input.push(0);
        for field in fields {
            // A field is a short string or 32 bytes; none comes near `u32`'s
            // range, and `HELLO_BODY_BYTES` bounds what either peer reads.
            let prefix = u32::try_from(field.len()).expect("a hello field fits a u32 length");
            input.extend_from_slice(&prefix.to_be_bytes());
            input.extend_from_slice(field);
        }
        input
    }
}

impl HelloFacts {
    /// The dialer's comparison of these facts with its own connection: the
    /// channel, then the host end against the address dialed, then the client
    /// end against the dialer's own local address, each address normalized.
    ///
    /// Pure and crypto-free, so both dialers compare by one rule. **The MAC
    /// is each dialer's to check first**, in constant time, with the HMAC
    /// this crate does not depend on: until it verifies, these facts are the
    /// word of whoever answered.
    pub fn check_connection(&self, expected: &HelloExpectation) -> Result<(), HelloMismatch> {
        if self.channel != expected.channel {
            return Err(HelloMismatch::Channel);
        }
        if self.host() != normalize_socket_address(expected.dialed) {
            return Err(HelloMismatch::Host);
        }
        if self.client() != normalize_socket_address(expected.local) {
            return Err(HelloMismatch::Client);
        }
        Ok(())
    }
}

/// The hello's answer: `{ "version": 1, "channel", "build", "launch",
/// "state", "host", "client", "mac" }`.
///
/// A reader ignores a field it does not know, which is what lets a later
/// build add one without a new route, and refuses any key given twice, known
/// or not: it reads through the one object-only reader the requests do.
#[derive(Clone, Debug, Eq, JsonSchema, PartialEq, Serialize)]
pub struct HelloAnswer {
    /// The hello shape's version: 1.
    version: u32,
    /// The facts the MAC covers.
    #[serde(flatten)]
    facts: HelloFacts,
    /// HMAC-SHA256 over [`HelloFacts::mac_input`], keyed by the labelled
    /// token's secret, as 64 lowercase hexadecimal digits.
    mac: String,
}

impl<'de> Deserialize<'de> for HelloAnswer {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// The answer's fields, read out of an object [`read_object`] took.
        #[derive(Deserialize)]
        struct Fields {
            version: u32,
            #[serde(flatten)]
            facts: HelloFacts,
            mac: String,
        }
        let object = read_object(deserializer)?;
        let Fields {
            version,
            facts,
            mac,
        } = Fields::deserialize(serde_json::Value::Object(object)).map_err(D::Error::custom)?;
        Ok(HelloAnswer {
            version,
            facts,
            mac,
        })
    }
}

impl HelloAnswer {
    /// The version this shape is.
    pub const VERSION: u32 = 1;

    /// The answer stating `facts`, under the `mac` the host computed over
    /// their [`mac_input`](HelloFacts::mac_input).
    pub fn new(facts: HelloFacts, mac: [u8; 32]) -> Self {
        HelloAnswer {
            version: HelloAnswer::VERSION,
            facts,
            mac: hex::encode(&mac),
        }
    }

    pub const fn version(&self) -> u32 {
        self.version
    }

    /// The facts the MAC covers, for the dialer to rebuild the MAC input
    /// from and to check.
    pub const fn facts(&self) -> &HelloFacts {
        &self.facts
    }

    /// The MAC, or `None` when what arrived is not 64 lowercase hexadecimal
    /// digits — which a dialer treats as a MAC that does not verify.
    pub fn mac(&self) -> Option<[u8; 32]> {
        hex::decode_array(&self.mac)
    }
}

/// What a dialer checks a hello answer against: its own channel, the socket
/// address it dialed, and its own local socket address on that connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HelloExpectation {
    pub channel: HelloChannel,
    pub dialed: SocketAddr,
    pub local: SocketAddr,
}

/// Why a dialer refused a hello answer. Each one withholds the bearer and
/// closes the connection.
///
/// Its `Display` names both possible causes and asserts neither, because a
/// dialer cannot tell them apart: the token may have changed since the dialer
/// read it, or something other than a norn host holds the port.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelloMismatch {
    /// The reply is not a hello answer, a refused hello, or a fail-closed
    /// answer: not a norn hello at all.
    NotAnAnswer,
    /// The MAC is missing or does not verify under the dialer's token.
    Mac,
    /// The answer names another channel.
    Channel,
    /// The answer's host end is not the address the dialer dialed.
    Host,
    /// The answer's client end is not the dialer's own end of this
    /// connection.
    Client,
}

impl fmt::Display for HelloMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self {
            HelloMismatch::NotAnAnswer => "the reply is not a hello answer",
            HelloMismatch::Mac => {
                "the answer's MAC does not verify under the token this dialer read"
            }
            HelloMismatch::Channel => "the answer names another channel",
            HelloMismatch::Host => "the answer's host address is not the address dialed",
            HelloMismatch::Client => {
                "the answer's client address is not this end of the connection"
            }
        };
        write!(f, "host verification failed: {what}. {BOTH_CAUSES}")
    }
}

/// The two causes every failed verification names, asserting neither.
const BOTH_CAUSES: &str = "Either the token changed since it was read, or something other than \
                           a norn host holds the port; the bearer was not sent";

/// Why a dial returned no verified connection. No bearer was sent in any.
///
/// Both dialers — `norn-client`'s and the serving crate's takeover dial —
/// answer this one type, so a failure reads the same from either. It carries
/// no transport type: a dialer writes its HTTP stack's error as text.
///
/// A refused hello and an answer that did not verify are both reported as a
/// failed verification naming both causes, since a refusal carries no MAC.
/// The one unverified answer reported for what it said is the fail-closed
/// one, and it is reported as a listener's word, not as a fact about any
/// file.
#[derive(Debug)]
pub enum HelloFailure {
    /// The address is not a loopback address, so it is not dialed.
    NotLoopback(SocketAddr),
    /// The token set holds no token to run the hello with.
    NoToken,
    /// The connection could not be made.
    Connect {
        address: SocketAddr,
        error: std::io::Error,
    },
    /// The connection failed during the hello.
    Exchange { address: SocketAddr, error: String },
    /// A listener at the address refused the hello (401
    /// `serve/hello-refused`). Unverified: a refusal carries no MAC.
    HelloRefused { address: SocketAddr },
    /// A listener at the address said its token file is unreadable (500
    /// `serve/token-file-unreadable`). Unverified, so it says nothing about
    /// any file for certain.
    ListenerSaysTokenFileUnreadable { address: SocketAddr },
    /// The answer did not verify.
    Verification {
        address: SocketAddr,
        mismatch: HelloMismatch,
    },
}

impl fmt::Display for HelloFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HelloFailure::NotLoopback(address) => write!(
                f,
                "{address} is not a loopback address; a dialer dials only loopback"
            ),
            HelloFailure::NoToken => {
                f.write_str("the token file holds no token to run the hello with")
            }
            HelloFailure::Connect { address, error } => {
                write!(f, "connecting to {address} failed: {error}")
            }
            HelloFailure::Exchange { address, error } => write!(
                f,
                "the connection to {address} failed during the hello: {error}"
            ),
            HelloFailure::HelloRefused { address } => write!(
                f,
                "{address}: host verification failed: a listener there refused the hello (401 \
                 `serve/hello-refused`), which carries no MAC. {BOTH_CAUSES}"
            ),
            HelloFailure::ListenerSaysTokenFileUnreadable { address } => write!(
                f,
                "a listener at {address} said the token file is unreadable; the answer is \
                 unverified"
            ),
            HelloFailure::Verification { address, mismatch } => {
                write!(f, "{address}: {mismatch}")
            }
        }
    }
}

impl std::error::Error for HelloFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            HelloFailure::Connect { error, .. } => Some(error),
            _ => None,
        }
    }
}

/// A reply to the hello, read from its status and body before anything in it
/// is trusted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HelloReply {
    /// A 200 whose body is a hello answer, still to be verified.
    Answer(HelloAnswer),
    /// A 401 `serve/hello-refused`: unverified, since a refusal carries no MAC.
    Refused,
    /// A 500 `serve/token-file-unreadable`: unverified, so it is what a
    /// listener at the endpoint *said*, not a fact about any file.
    TokenFileUnreadable,
    /// Anything else, which no norn host answers to a hello.
    Unrecognized,
}

impl HelloReply {
    /// Read a reply from its HTTP status code and its body.
    pub fn read(status: u16, body: &[u8]) -> Self {
        match status {
            200 => serde_json::from_slice::<HelloAnswer>(body)
                .map_or(HelloReply::Unrecognized, HelloReply::Answer),
            401 | 500 => match serde_json::from_slice::<ErrorEnvelope>(body) {
                Ok(envelope) => match (status, envelope.code()) {
                    (401, ReasonCode::ServeHelloRefused) => HelloReply::Refused,
                    (500, ReasonCode::ServeTokenFileUnreadable) => HelloReply::TokenFileUnreadable,
                    _ => HelloReply::Unrecognized,
                },
                Err(_) => HelloReply::Unrecognized,
            },
            _ => HelloReply::Unrecognized,
        }
    }
}

/// Whether a dialer may dial `address`: a loopback address, once normalized.
///
/// Every dialer connects only to a resolved loopback socket address. A
/// `SocketAddr` already rules out a hostname; this rules out the rest.
pub fn is_dialable(address: SocketAddr) -> bool {
    normalize_socket_address(address).ip().is_loopback()
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    fn nonce() -> Nonce {
        let mut bytes = [0u8; 32];
        for (slot, value) in bytes.iter_mut().zip(0u8..) {
            *slot = value;
        }
        Nonce::from_bytes(bytes)
    }

    fn address(text: &str) -> SocketAddr {
        text.parse().expect("a socket address")
    }

    /// The contract's own example answer.
    fn facts() -> HelloFacts {
        HelloFacts::new(
            HelloChannel::Dev,
            BuildIdentity::new("0.1.0+3f2a9c1e"),
            HelloLaunch::Explicit,
            HelloState::Serving,
            address("127.0.0.1:28472"),
            address("127.0.0.1:54121"),
        )
    }

    /// The MAC input, written out by hand from the contract: every length is
    /// a literal here, not a computed one, so a change to the builder's order,
    /// prefix width, byte order or separator fails this.
    #[test]
    fn the_mac_input_is_the_bytes_the_contract_spells() {
        let mut expected: Vec<u8> = Vec::new();
        expected.extend_from_slice(b"norn-hello-v1\x00");
        expected.extend_from_slice(b"\x00\x00\x00\x20");
        expected.extend_from_slice(&[
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
            0x1c, 0x1d, 0x1e, 0x1f,
        ]);
        expected.extend_from_slice(b"\x00\x00\x00\x03dev");
        expected.extend_from_slice(b"\x00\x00\x00\x0e0.1.0+3f2a9c1e");
        expected.extend_from_slice(b"\x00\x00\x00\x08explicit");
        expected.extend_from_slice(b"\x00\x00\x00\x07serving");
        expected.extend_from_slice(b"\x00\x00\x00\x0f127.0.0.1:28472");
        expected.extend_from_slice(b"\x00\x00\x00\x0f127.0.0.1:54121");

        assert_eq!(facts().mac_input(&nonce()), expected);
    }

    /// An IPv6 end is written in brackets in the MAC input as on the wire.
    #[test]
    fn an_ipv6_end_is_written_in_brackets_in_the_mac_input() {
        let facts = HelloFacts::new(
            HelloChannel::Live,
            BuildIdentity::new("b"),
            HelloLaunch::Auto,
            HelloState::Draining,
            address("[::1]:28471"),
            address("[::1]:50000"),
        );
        let input = facts.mac_input(&nonce());
        let tail: &[u8] = b"\x00\x00\x00\x0b[::1]:28471\x00\x00\x00\x0b[::1]:50000";
        assert!(input.ends_with(tail), "{input:?}");
    }

    /// Every covered field is covered: changing any one of them, or the
    /// nonce, changes the input.
    #[test]
    fn changing_any_covered_field_changes_the_mac_input() {
        let base = facts().mac_input(&nonce());
        let build = BuildIdentity::new("0.1.0+3f2a9c1e");
        let host = address("127.0.0.1:28472");
        let client = address("127.0.0.1:54121");
        let (dev, explicit, serving) = (
            HelloChannel::Dev,
            HelloLaunch::Explicit,
            HelloState::Serving,
        );
        let variants = [
            (
                "channel",
                HelloFacts::new(HelloChannel::Live, build, explicit, serving, host, client),
            ),
            (
                "build",
                HelloFacts::new(
                    dev,
                    BuildIdentity::new("0.1.0+3f2a9c1e-dirty"),
                    explicit,
                    serving,
                    host,
                    client,
                ),
            ),
            (
                "launch",
                HelloFacts::new(dev, build, HelloLaunch::Auto, serving, host, client),
            ),
            (
                "state",
                HelloFacts::new(dev, build, explicit, HelloState::Draining, host, client),
            ),
            (
                "host",
                HelloFacts::new(
                    dev,
                    build,
                    explicit,
                    serving,
                    address("127.0.0.1:28471"),
                    client,
                ),
            ),
            (
                "client",
                HelloFacts::new(
                    dev,
                    build,
                    explicit,
                    serving,
                    host,
                    address("127.0.0.1:54122"),
                ),
            ),
        ];
        for (field, facts) in variants {
            assert_ne!(facts.mac_input(&nonce()), base, "{field} is not covered");
        }
        let mut other = *nonce().as_bytes();
        other[31] ^= 1;
        assert_ne!(
            facts().mac_input(&Nonce::from_bytes(other)),
            base,
            "the nonce is not covered"
        );
    }

    /// The host and the dialer build the input from the same answer: what
    /// the host serializes, the dialer reads back to the same facts and the
    /// same bytes. The answer's `version` and `mac` are not among them.
    #[test]
    fn a_dialer_rebuilds_the_hosts_mac_input_from_the_answer_it_reads() {
        let sent = HelloAnswer::new(facts(), [7; 32]);
        let json = serde_json::to_string(&sent).expect("an answer as JSON");
        let read: HelloAnswer = serde_json::from_str(&json).expect("reading the answer");
        assert_eq!(read, sent);
        assert_eq!(
            read.facts().mac_input(&nonce()),
            facts().mac_input(&nonce())
        );
        assert_eq!(read.mac(), Some([7; 32]));

        let other_mac = HelloAnswer::new(facts(), [9; 32]);
        assert_eq!(
            other_mac.facts().mac_input(&nonce()),
            sent.facts().mac_input(&nonce()),
            "the MAC does not cover itself"
        );
    }

    #[test]
    fn an_answer_is_written_as_the_contract_spells_it() {
        let answer = HelloAnswer::new(facts(), [0xab; 32]);
        assert_eq!(
            serde_json::to_string(&answer).expect("an answer as JSON"),
            format!(
                concat!(
                    r#"{{"version":1,"channel":"dev","build":"0.1.0+3f2a9c1e","#,
                    r#""launch":"explicit","state":"serving","host":"127.0.0.1:28472","#,
                    r#""client":"127.0.0.1:54121","mac":"{}"}}"#
                ),
                "ab".repeat(32)
            )
        );
    }

    /// A v1 reader ignores a field it does not know, which is how a later
    /// build adds one to the answer without a new route.
    #[test]
    fn an_answer_reader_ignores_a_field_it_does_not_know() {
        let json = concat!(
            r#"{"version":1,"channel":"live","build":"b","launch":"auto","#,
            r#""state":"draining","host":"[::1]:28471","client":"[::1]:50000","#,
            r#""mac":"00","uptime_seconds":12}"#
        );
        let read: HelloAnswer = serde_json::from_str(json).expect("an answer with a new field");
        assert_eq!(read.facts().channel(), HelloChannel::Live);
        assert_eq!(read.facts().launch(), HelloLaunch::Auto);
        assert_eq!(read.facts().state(), HelloState::Draining);
        assert_eq!(read.facts().host(), address("[::1]:28471"));
        assert_eq!(
            read.mac(),
            None,
            "a MAC that is not 64 hex digits reads as none"
        );
    }

    #[test]
    fn a_mapped_address_is_normalized_to_ipv4_and_an_ipv6_one_loses_its_scope() {
        let mapped = SocketAddr::V6(SocketAddrV6::new(
            Ipv4Addr::LOCALHOST.to_ipv6_mapped(),
            28472,
            0,
            0,
        ));
        assert_eq!(normalize_socket_address(mapped), address("127.0.0.1:28472"));
        let scoped = SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 28472, 9, 3));
        assert_eq!(normalize_socket_address(scoped), address("[::1]:28472"));
        assert_eq!(
            normalize_socket_address(address("127.0.0.1:1")),
            address("127.0.0.1:1")
        );

        // The facts hold the normalized form, written that way, and a mapped
        // form read off the wire is normalized too.
        let facts = HelloFacts::new(
            HelloChannel::Dev,
            BuildIdentity::new("b"),
            HelloLaunch::Explicit,
            HelloState::Serving,
            mapped,
            scoped,
        );
        assert_eq!(facts.host(), address("127.0.0.1:28472"));
        let json = serde_json::to_value(&facts).expect("facts as JSON");
        assert_eq!(json["host"], "127.0.0.1:28472");
        assert_eq!(json["client"], "[::1]:28472");
        let read: HelloFacts = serde_json::from_value(serde_json::json!({
            "channel": "dev", "build": "b", "launch": "explicit", "state": "serving",
            "host": "[::ffff:127.0.0.1]:28472", "client": "[::1]:28472",
        }))
        .expect("facts with a mapped host");
        assert_eq!(read, facts);
    }

    #[test]
    fn an_address_that_is_not_ip_and_port_does_not_read() {
        for host in ["localhost:28472", "127.0.0.1", "::1:28472", ""] {
            let read = serde_json::from_value::<HelloFacts>(serde_json::json!({
                "channel": "dev", "build": "b", "launch": "explicit", "state": "serving",
                "host": host, "client": "127.0.0.1:1",
            }));
            assert!(read.is_err(), "`{host}` read as an address");
        }
    }

    #[test]
    fn the_vocabularies_spell_on_the_wire_what_the_mac_input_holds() {
        for channel in [HelloChannel::Dev, HelloChannel::Live] {
            assert_eq!(
                serde_json::to_value(channel).expect("json"),
                channel.as_str()
            );
        }
        for launch in [HelloLaunch::Explicit, HelloLaunch::Auto] {
            assert_eq!(serde_json::to_value(launch).expect("json"), launch.as_str());
        }
        for state in [HelloState::Serving, HelloState::Draining] {
            assert_eq!(serde_json::to_value(state).expect("json"), state.as_str());
        }
    }

    #[test]
    fn a_request_is_written_as_the_contract_spells_it() {
        let request = HelloRequest::new("default", &nonce());
        let json = serde_json::to_string(&request).expect("a request as JSON");
        assert_eq!(
            json,
            r#"{"label":"default","nonce":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"}"#
        );
        let read: HelloRequest = serde_json::from_str(&json).expect("reading it back");
        assert_eq!(read.label(), "default");
        assert_eq!(read.nonce(), Some(nonce()));
    }

    /// A body of the request's shape parses whatever its label and nonce say,
    /// so the host can answer a bad nonce as a refused hello; a body of
    /// another shape does not parse at all.
    #[test]
    fn a_malformed_nonce_parses_as_a_request_and_reads_as_no_nonce() {
        let digits = nonce().to_hex();
        for bad in [
            digits.to_uppercase(),
            digits[..62].to_string(),
            format!("{digits}00"),
            format!("{}zz", &digits[..62]),
            String::new(),
        ] {
            let read: HelloRequest =
                serde_json::from_value(serde_json::json!({ "label": "default", "nonce": bad }))
                    .expect("a request whose nonce does not read");
            assert_eq!(read.nonce(), None, "`{bad}` read as a nonce");
        }

        let unknown_field: HelloRequest = serde_json::from_value(serde_json::json!({
            "label": "Not A Label", "nonce": digits, "extra": true,
        }))
        .expect("a request with a field this build does not know");
        assert_eq!(unknown_field.label(), "Not A Label");

        for not_a_request in [
            serde_json::json!({ "label": "default" }),
            serde_json::json!({ "nonce": digits }),
            serde_json::json!({ "label": 1, "nonce": digits }),
            serde_json::json!([]),
            serde_json::json!(["default", digits]),
            serde_json::json!(null),
        ] {
            assert!(
                serde_json::from_value::<HelloRequest>(not_a_request.clone()).is_err(),
                "{not_a_request} parsed as a hello request"
            );
        }
    }

    fn expectation() -> HelloExpectation {
        HelloExpectation {
            channel: HelloChannel::Dev,
            dialed: address("127.0.0.1:28472"),
            local: address("127.0.0.1:54121"),
        }
    }

    #[test]
    fn facts_that_state_this_connection_pass() {
        assert_eq!(facts().check_connection(&expectation()), Ok(()));
    }

    #[test]
    fn facts_naming_another_channel_or_either_other_end_are_refused() {
        let other_channel = HelloExpectation {
            channel: HelloChannel::Live,
            ..expectation()
        };
        assert_eq!(
            facts().check_connection(&other_channel),
            Err(HelloMismatch::Channel)
        );
        let other_port = HelloExpectation {
            dialed: address("127.0.0.1:28471"),
            ..expectation()
        };
        assert_eq!(
            facts().check_connection(&other_port),
            Err(HelloMismatch::Host)
        );
        // The client's own port on a second loopback address: only the IP
        // differs, and that is a different end.
        let other_address = HelloExpectation {
            local: address("127.0.0.2:54121"),
            ..expectation()
        };
        assert_eq!(
            facts().check_connection(&other_address),
            Err(HelloMismatch::Client)
        );
        let other_source_port = HelloExpectation {
            local: address("127.0.0.1:54122"),
            ..expectation()
        };
        assert_eq!(
            facts().check_connection(&other_source_port),
            Err(HelloMismatch::Client)
        );
    }

    /// The dialer's own addresses are normalized the way the host's are, so
    /// a dual-stack dialer's mapped spelling compares equal.
    #[test]
    fn the_dialers_addresses_are_normalized_before_they_are_compared() {
        let mapped = |port| {
            SocketAddr::V6(SocketAddrV6::new(
                Ipv4Addr::LOCALHOST.to_ipv6_mapped(),
                port,
                0,
                0,
            ))
        };
        let expected = HelloExpectation {
            channel: HelloChannel::Dev,
            dialed: mapped(28472),
            local: mapped(54121),
        };
        assert_eq!(facts().check_connection(&expected), Ok(()));
    }

    /// The contract's example answer fits the body cap with room to spare,
    /// so the cap refuses only what no host writes.
    #[test]
    fn an_answer_fits_the_hello_body_cap() {
        let answer = HelloAnswer::new(facts(), [0xab; 32]);
        let bytes = serde_json::to_vec(&answer).expect("json");
        assert!(bytes.len() * 4 < HELLO_BODY_BYTES, "{}", bytes.len());
    }

    #[test]
    fn a_mismatch_says_verification_failed_and_names_both_causes() {
        for mismatch in [
            HelloMismatch::NotAnAnswer,
            HelloMismatch::Mac,
            HelloMismatch::Channel,
            HelloMismatch::Host,
            HelloMismatch::Client,
        ] {
            let text = mismatch.to_string();
            assert!(text.starts_with("host verification failed"), "{text}");
            assert!(
                text.contains("the token changed since it was read"),
                "{text}"
            );
            assert!(
                text.contains("something other than a norn host holds the port"),
                "{text}"
            );
        }
    }

    /// A 401 carries no MAC, so a listener that refused the hello is no more
    /// verified than one whose MAC failed: the failure reads the same.
    #[test]
    fn a_refused_hello_says_verification_failed_and_names_both_causes() {
        let address = address("127.0.0.1:28472");
        for failure in [
            HelloFailure::HelloRefused { address },
            HelloFailure::Verification {
                address,
                mismatch: HelloMismatch::Mac,
            },
        ] {
            let text = failure.to_string();
            assert!(text.starts_with("127.0.0.1:28472: "), "{text}");
            assert!(text.contains("host verification failed"), "{text}");
            assert!(
                text.contains("the token changed since it was read"),
                "{text}"
            );
            assert!(
                text.contains("something other than a norn host holds the port"),
                "{text}"
            );
            assert!(text.contains("the bearer was not sent"), "{text}");
        }
        let refused = HelloFailure::HelloRefused { address }.to_string();
        assert!(refused.contains("serve/hello-refused"), "{refused}");
    }

    /// The one unverified answer reported for what it said rather than as a
    /// failed verification.
    #[test]
    fn a_listener_saying_the_token_file_is_unreadable_is_reported_as_its_word() {
        let address = address("127.0.0.1:28472");
        let text = HelloFailure::ListenerSaysTokenFileUnreadable { address }.to_string();
        assert!(
            text.contains("a listener at 127.0.0.1:28472 said the token file is unreadable"),
            "{text}"
        );
        assert!(text.contains("unverified"), "{text}");
        assert!(!text.contains("verification failed"), "{text}");
    }

    #[test]
    fn a_reply_is_read_by_its_status_and_its_code() {
        let answer = HelloAnswer::new(facts(), [1; 32]);
        let body = serde_json::to_vec(&answer).expect("json");
        assert_eq!(HelloReply::read(200, &body), HelloReply::Answer(answer));
        assert_eq!(HelloReply::read(200, b"{}"), HelloReply::Unrecognized);
        assert_eq!(HelloReply::read(201, &body), HelloReply::Unrecognized);

        let envelope =
            |detail| serde_json::to_vec(&ErrorEnvelope::new("refused", detail)).expect("json");
        let refused = envelope(crate::ErrorDetail::hello_refused());
        let unreadable = envelope(crate::ErrorDetail::token_file_unreadable());
        assert_eq!(HelloReply::read(401, &refused), HelloReply::Refused);
        assert_eq!(
            HelloReply::read(500, &unreadable),
            HelloReply::TokenFileUnreadable
        );
        // A code under the other status, another code, and no envelope at
        // all are none of those.
        assert_eq!(HelloReply::read(500, &refused), HelloReply::Unrecognized);
        assert_eq!(HelloReply::read(401, &unreadable), HelloReply::Unrecognized);
        let unauthorized = envelope(crate::ErrorDetail::unauthorized());
        assert_eq!(
            HelloReply::read(401, &unauthorized),
            HelloReply::Unrecognized
        );
        assert_eq!(HelloReply::read(404, b""), HelloReply::Unrecognized);
        assert_eq!(HelloReply::read(401, b"no"), HelloReply::Unrecognized);
    }

    #[test]
    fn only_a_loopback_address_is_dialable() {
        assert!(is_dialable(address("127.0.0.1:28472")));
        assert!(is_dialable(address("127.0.0.2:28472")));
        assert!(is_dialable(address("[::1]:28472")));
        assert!(is_dialable(SocketAddr::V6(SocketAddrV6::new(
            Ipv4Addr::LOCALHOST.to_ipv6_mapped(),
            1,
            0,
            0
        ))));
        assert!(!is_dialable(address("0.0.0.0:28472")));
        assert!(!is_dialable(address("192.0.2.1:28472")));
        assert!(!is_dialable(address("[::]:28472")));
    }

    #[test]
    fn a_nonce_round_trips_through_its_hex() {
        assert_eq!(Nonce::from_hex(&nonce().to_hex()), Some(nonce()));
        assert_eq!(nonce().to_hex().len(), 64);
    }

    /// A key given twice is refused rather than read as whichever came last,
    /// as the answer's reader refuses one: the request and the answer read one
    /// grammar of objects.
    #[test]
    fn a_request_refuses_a_key_given_twice() {
        let digits = nonce().to_hex();
        for twice in [
            format!(r#"{{"label":"a","label":"b","nonce":"{digits}"}}"#),
            format!(r#"{{"label":"a","nonce":"{digits}","nonce":"{digits}"}}"#),
        ] {
            assert!(
                serde_json::from_str::<HelloRequest>(&twice).is_err(),
                "`{twice}` read as a hello request"
            );
        }
        let answer = serde_json::to_string(&HelloAnswer::new(facts(), [1; 32])).expect("json");
        for twice in [
            answer.replacen(r#""mac":"#, r#""mac":"00","mac":"#, 1),
            // A key this build does not know is ignored, but not given twice:
            // each key once is the grammar, known or not.
            answer.replacen('{', r#"{"extra":0,"extra":{"a":[1,true]},"#, 1),
        ] {
            assert!(
                serde_json::from_str::<HelloAnswer>(&twice).is_err(),
                "{twice}"
            );
        }
        let extra = answer.replacen('{', r#"{"extra":{"a":[1,true]},"#, 1);
        assert!(
            serde_json::from_str::<HelloAnswer>(&extra).is_ok(),
            "an unknown key given once is ignored: {extra}"
        );
    }

    /// A MAC is 64 digits exactly: a decode of any other even width, short or
    /// long, is no MAC rather than the first 32 bytes of one.
    #[test]
    fn a_mac_of_any_width_but_sixty_four_digits_reads_as_none() {
        let json = serde_json::to_value(HelloAnswer::new(facts(), [0x5a; 32])).expect("json");
        for digits in [62, 66, 128] {
            let mut answer = json.clone();
            answer["mac"] = serde_json::Value::String("5a".repeat(digits / 2));
            let read: HelloAnswer = serde_json::from_value(answer).expect("an answer");
            assert_eq!(read.mac(), None, "a {digits}-digit MAC read as one");
        }
        let read: HelloAnswer = serde_json::from_value(json).expect("an answer");
        assert_eq!(read.mac(), Some([0x5a; 32]));
    }
}
