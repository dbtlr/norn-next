//! One POSTed JSON-RPC message: its envelope read, its revision told, and its
//! answer composed.
//!
//! **Pure translation.** [`handle`] takes the header strings and the body the
//! transport received, and answers either a finished [`Answer`] — its HTTP
//! status and bytes, which the transport writes as given — or a [`Call`] for
//! the transport to run through the wire route's dispatch path. A call's
//! outcome is composed by its [`Reply`]. Nothing here holds state across
//! messages, so neither revision keeps a session.
//!
//! **No tree of the arguments.** The envelope is read as a plain struct whose
//! fields are `RawValue`s, and `params` as another; a call's `arguments` stay
//! the bytes they arrived as. A report is composed into a result from its own
//! bytes: `structuredContent` is the report verbatim, and the text block is
//! those bytes escaped once as a JSON string.

use std::borrow::Cow;
use std::sync::Arc;

use norn_wire::{ApplyMode, ErrorDetail, ErrorEnvelope, FaultClass, ReasonCode, Verb};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;

use crate::{PreparedToolLists, tool_name};

/// The revision every 2025-era message is served as.
pub const LEGACY_VERSION: &str = "2025-11-25";

/// The modern revision, the one per-request metadata identifies.
pub const MODERN_VERSION: &str = "2026-07-28";

/// The revisions earlier than [`LEGACY_VERSION`]: with it, the legacy set,
/// whose header marks a request as 2025-era.
const EARLIER_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// How long a client may keep a 2026-07-28 tool list or discovery result. The
/// list depends only on the build, so this bounds how long a client keeps a
/// stale one after a restart.
pub const TTL_MS: u64 = 300_000;

/// JSON-RPC error codes this route answers with.
pub mod codes {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const HEADER_MISMATCH: i32 = -32020;
    pub const UNSUPPORTED_VERSION: i32 = -32022;
}

/// The protocol revision a message is served under.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Revision {
    /// 2025-11-25, negotiated by `initialize`.
    Legacy,
    /// 2026-07-28, identified on every request by its metadata.
    Modern,
}

/// The request headers classification reads, as raw bytes; `None` where the
/// request did not carry one.
#[derive(Clone, Copy, Debug, Default)]
pub struct Headers<'a> {
    /// `MCP-Protocol-Version`.
    pub protocol_version: Option<&'a [u8]>,
    /// `Mcp-Method`.
    pub method: Option<&'a [u8]>,
    /// `Mcp-Name`.
    pub name: Option<&'a [u8]>,
}

/// A finished answer: the HTTP status and the body bytes. An empty body is a
/// notification's 202, which carries no content type.
#[derive(Debug, Eq, PartialEq)]
pub struct Answer {
    pub status: u16,
    pub body: Vec<u8>,
    /// The JSON-RPC error code this answer carries, if it is an error.
    pub error_code: Option<i32>,
    /// The wire reason code carried in band by a tool failure.
    pub wire_error: Option<ReasonCode>,
}

/// What a message comes to.
#[derive(Debug)]
pub enum Message<'a> {
    /// Answered here, from constants and the prepared bytes.
    Answered(Answer),
    /// A `tools/call` naming a served tool, for the wire route's dispatch
    /// path.
    Call(Call<'a>),
}

/// A `tools/call` resolved to its tool.
#[derive(Debug)]
pub struct Call<'a> {
    /// The verb the tool runs.
    pub verb: Verb,
    /// The mode the tool's name fixes, for a mode tool.
    pub mode: Option<ApplyMode>,
    /// The call's `arguments`: an object, as the bytes it arrived as, or `{}`
    /// where none was given.
    pub arguments: &'a [u8],
    /// Where its outcome is addressed.
    pub reply: Reply,
}

impl Call<'_> {
    /// Check the caller's mode before dispatch. The splice can then run on
    /// the pool without scanning the arguments for the key again.
    pub fn mode_splice(&self) -> Result<ModeSplice, Answer> {
        ModeSplice::check(self.arguments, self.mode)
            .map_err(|ModeGiven| self.reply.mode_given(self.verb))
    }
}

/// Who a call's outcome is addressed to: its request id and its revision.
/// Owned, so it crosses to the thread that composes the outcome.
#[derive(Clone, Debug)]
pub struct Reply {
    /// The request id, as the JSON it arrived as.
    id: Box<str>,
    revision: Revision,
    /// The server identity's JSON, which every 2026-07-28 result carries.
    server_info: Arc<str>,
}

/// One served tool's name and what it runs: the part of a tool definition a
/// call is resolved by.
#[derive(Clone, Debug)]
pub struct ToolRoute {
    pub name: String,
    pub verb: Verb,
    pub mode: Option<ApplyMode>,
}

/// A caller named `mode` in a mode tool's arguments: the tool's name fixes it.
#[derive(Debug, Eq, PartialEq)]
pub struct ModeGiven;

/// A mode splice checked against a call's original argument bytes.
#[derive(Clone, Copy, Debug)]
pub struct ModeSplice(Option<ApplyMode>);

/// Read one message and answer it, or resolve it to a call.
pub fn handle<'a>(
    prepared: &PreparedToolLists,
    headers: Headers<'_>,
    body: &'a [u8],
) -> Message<'a> {
    let envelope = match Envelope::read(body) {
        Ok(envelope) => envelope,
        Err(answer) => return Message::Answered(answer),
    };
    let Some(id) = envelope.id else {
        if headers.protocol_version.is_some_and(|version| {
            version != LEGACY_VERSION.as_bytes() && version != MODERN_VERSION.as_bytes()
        }) {
            return Message::Answered(error(
                400,
                None,
                codes::INVALID_REQUEST,
                "this server speaks 2025-11-25 and 2026-07-28",
                None,
            ));
        }
        return Message::Answered(Answer {
            status: 202,
            body: Vec::new(),
            error_code: None,
            wire_error: None,
        });
    };
    let request = Request {
        id,
        method: &envelope.method,
        params: &envelope.params,
        headers,
        server_info: &prepared.server_info,
    };
    let meta_version = envelope.params.meta.protocol_version;
    let header_names_modern = headers
        .protocol_version
        .is_some_and(|version| !is_legacy(version));
    if meta_version.is_some() || header_names_modern {
        request.modern(prepared)
    } else {
        request.legacy(prepared)
    }
}

/// Whether `version` names a legacy revision: 2025-11-25 or an earlier one.
fn is_legacy(version: &[u8]) -> bool {
    version == LEGACY_VERSION.as_bytes()
        || EARLIER_VERSIONS
            .iter()
            .any(|earlier| version == earlier.as_bytes())
}

/// A request's envelope, read as raw fields so that each is checked on its
/// own and the id echoed wherever it could be read.
struct Envelope<'a> {
    /// The id's JSON; `None` for a notification.
    id: Option<&'a str>,
    method: String,
    params: Params<'a>,
}

#[derive(Deserialize)]
struct RawEnvelope<'a> {
    #[serde(default, borrow, deserialize_with = "present")]
    jsonrpc: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    id: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    method: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    params: Option<&'a RawValue>,
}

/// The fields of `params` any served method reads. Every other field is
/// skipped unparsed, and a call's `arguments` stay raw.
#[derive(Default, Deserialize)]
struct Params<'a> {
    #[serde(rename = "_meta", default, borrow, deserialize_with = "present")]
    meta_raw: Option<&'a RawValue>,
    #[serde(skip)]
    meta: Meta<'a>,
    #[serde(default, borrow, deserialize_with = "present")]
    name: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    arguments: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    cursor: Option<&'a RawValue>,
}

/// The per-request metadata a 2026-07-28 request carries.
#[derive(Default, Deserialize)]
struct Meta<'a> {
    #[serde(
        rename = "io.modelcontextprotocol/protocolVersion",
        default,
        borrow,
        deserialize_with = "present"
    )]
    protocol_version: Option<&'a RawValue>,
    #[serde(
        rename = "io.modelcontextprotocol/clientCapabilities",
        default,
        borrow,
        deserialize_with = "present"
    )]
    client_capabilities: Option<&'a RawValue>,
}

/// A field that is present, `null` included: `Option`'s own reading takes
/// `null` for absent.
fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<&'de RawValue>, D::Error> {
    <&RawValue>::deserialize(deserializer).map(Some)
}

/// The string a raw value holds, if it holds one.
fn string(raw: &RawValue) -> Option<String> {
    serde_json::from_str::<String>(raw.get()).ok()
}

fn is_object(raw: &RawValue) -> bool {
    raw.get().starts_with('{')
}

impl<'a> Envelope<'a> {
    fn read(body: &'a [u8]) -> Result<Self, Answer> {
        let structure =
            |id: Option<&str>, message: &str| error(400, id, codes::INVALID_REQUEST, message, None);
        if body
            .iter()
            .find(|byte| !byte.is_ascii_whitespace())
            .is_some_and(|byte| *byte == b'[')
        {
            if serde_json::from_slice::<serde::de::IgnoredAny>(body).is_err() {
                return Err(not_json());
            }
            return Err(structure(
                None,
                "a batch is not a request: send one message per POST",
            ));
        }
        let raw: RawEnvelope<'a> = match serde_json::from_slice(body) {
            Ok(raw) => raw,
            Err(_) => {
                // Not JSON at all, or JSON that is not one request object.
                if serde_json::from_slice::<serde::de::IgnoredAny>(body).is_err() {
                    return Err(not_json());
                }
                return Err(structure(
                    None,
                    "the message is not a JSON-RPC request object",
                ));
            }
        };
        let id = match raw.id {
            None => None,
            Some(id) if is_request_id(id.get()) => Some(id.get()),
            Some(_) => return Err(structure(None, "`id` must be a string or an integer")),
        };
        if raw.jsonrpc.and_then(string).as_deref() != Some("2.0") {
            return Err(structure(id, "`jsonrpc` must be \"2.0\""));
        }
        let Some(method) = raw.method.and_then(string) else {
            return Err(structure(id, "`method` must be a string"));
        };
        let params = match raw.params {
            None => Params::default(),
            Some(params) if is_object(params) => {
                let Ok(mut params) = serde_json::from_str::<Params<'a>>(params.get()) else {
                    return Err(structure(id, "`params` names a field twice"));
                };
                // Metadata that is present but does not read is a structure
                // fault, refused before the revision is told: read as absent,
                // it would carry a 2026-07-28 request onto the 2025-era path
                // past its header checks. Only an object reads, since a
                // struct would also read from an array.
                if let Some(meta) = params.meta_raw {
                    params.meta = match serde_json::from_str::<Meta<'a>>(meta.get()) {
                        Ok(read) if is_object(meta) => read,
                        _ => {
                            return Err(structure(
                                id,
                                "`params._meta` must be an object that names each field once",
                            ));
                        }
                    };
                }
                params
            }
            Some(_) => return Err(structure(id, "`params` must be an object")),
        };
        Ok(Envelope { id, method, params })
    }
}

/// Whether `raw`, a JSON value, is a string or an integer: both revisions'
/// `RequestId`, which excludes `null`.
fn is_request_id(raw: &str) -> bool {
    raw.starts_with('"')
        || ((raw.starts_with('-') || raw.starts_with(|c: char| c.is_ascii_digit()))
            && !raw.contains(['.', 'e', 'E']))
}

fn not_json() -> Answer {
    error(400, None, codes::PARSE_ERROR, "the body is not JSON", None)
}

/// A request, its envelope read.
struct Request<'r, 'a> {
    id: &'a str,
    method: &'r str,
    params: &'r Params<'a>,
    headers: Headers<'r>,
    server_info: &'r Arc<str>,
}

impl<'a> Request<'_, 'a> {
    /// A 2026-07-28 request: its metadata, then its headers, then its method.
    fn modern(&self, prepared: &PreparedToolLists) -> Message<'a> {
        let id = Some(self.id);
        let meta = &self.params.meta;
        let version = meta.protocol_version.and_then(string);
        let capabilities = meta.client_capabilities.filter(|raw| is_object(raw));
        let (Some(version), Some(_)) = (version, capabilities) else {
            return answered(error(
                400,
                id,
                codes::INVALID_PARAMS,
                "a 2026-07-28 request carries `io.modelcontextprotocol/protocolVersion` and \
                 `io.modelcontextprotocol/clientCapabilities` in `params._meta`",
                None,
            ));
        };
        if version != MODERN_VERSION {
            let data = serde_json::to_string(&Unsupported {
                supported: [MODERN_VERSION, LEGACY_VERSION],
                requested: &version,
            })
            .expect("version data serializes");
            return answered(error(
                400,
                id,
                codes::UNSUPPORTED_VERSION,
                "this server does not speak the requested protocol version",
                Some(&data),
            ));
        }
        let mismatch =
            |message: &str| answered(error(400, id, codes::HEADER_MISMATCH, message, None));
        if self.headers.protocol_version != Some(version.as_bytes()) {
            return mismatch(
                "`MCP-Protocol-Version` is missing or differs from the request's metadata",
            );
        }
        if self.headers.method != Some(self.method.as_bytes()) {
            return mismatch("`Mcp-Method` is missing or differs from the request's method");
        }
        if self.method == "tools/call" {
            let name = self.params.name.and_then(string);
            let header = self.headers.name.and_then(decode_header);
            if header.is_none() || header.as_deref() != name.as_deref().map(str::as_bytes) {
                return mismatch("`Mcp-Name` is missing or differs from the tool's name");
            }
        }
        match self.method {
            "server/discover" => answered(result(
                self.id,
                Revision::Modern,
                self.server_info,
                &discover_result(),
            )),
            "tools/list" => self.tools_list(&prepared.modern_body),
            "tools/call" => self.tools_call(Revision::Modern, prepared),
            _ => answered(error(
                404,
                id,
                codes::METHOD_NOT_FOUND,
                "this server serves no such method under 2026-07-28",
                None,
            )),
        }
    }

    /// A 2025-era request: `initialize` negotiates; every other request
    /// carries the 2025-11-25 header.
    fn legacy(&self, prepared: &PreparedToolLists) -> Message<'a> {
        let id = Some(self.id);
        if self.method == "initialize" {
            return answered(result(
                self.id,
                Revision::Legacy,
                self.server_info,
                &initialize_result(self.server_info),
            ));
        }
        match self.headers.protocol_version {
            None => {
                return answered(error(
                    400,
                    id,
                    codes::HEADER_MISMATCH,
                    "a request after `initialize` carries `MCP-Protocol-Version`",
                    None,
                ));
            }
            Some(version) if version != LEGACY_VERSION.as_bytes() => {
                return answered(error(
                    400,
                    id,
                    codes::INVALID_REQUEST,
                    "this server speaks 2025-11-25 and 2026-07-28; `initialize` negotiates",
                    None,
                ));
            }
            Some(_) => {}
        }
        match self.method {
            "ping" => answered(result(self.id, Revision::Legacy, self.server_info, b"{}")),
            "tools/list" => self.tools_list(&prepared.legacy_body),
            "tools/call" => self.tools_call(Revision::Legacy, prepared),
            _ => answered(error(
                200,
                id,
                codes::METHOD_NOT_FOUND,
                "this server serves no such method under 2025-11-25",
                None,
            )),
        }
    }

    /// `tools/list`: the prepared bytes, in one page.
    fn tools_list(&self, body: &[u8]) -> Message<'a> {
        if self.params.cursor.is_some() {
            return answered(error(
                200,
                Some(self.id),
                codes::INVALID_PARAMS,
                "no cursor was issued: the tool list is one page",
                None,
            ));
        }
        answered(response(self.id, body))
    }

    /// `tools/call`: the tool resolved and its arguments checked.
    fn tools_call(&self, revision: Revision, prepared: &PreparedToolLists) -> Message<'a> {
        let invalid = |message: &str| {
            answered(error(
                200,
                Some(self.id),
                codes::INVALID_PARAMS,
                message,
                None,
            ))
        };
        let Some(tool) = self
            .params
            .name
            .and_then(string)
            .and_then(|name| prepared.tools.iter().find(|tool| tool.name == name))
        else {
            return invalid("this server serves no tool by that name; `tools/list` names them");
        };
        let arguments = match self.params.arguments {
            None => b"{}".as_slice(),
            Some(arguments) if is_object(arguments) => arguments.get().as_bytes(),
            Some(_) => return invalid("`arguments` must be an object"),
        };
        Message::Call(Call {
            verb: tool.verb,
            mode: tool.mode,
            arguments,
            reply: Reply {
                id: self.id.into(),
                revision,
                server_info: Arc::clone(self.server_info),
            },
        })
    }
}

#[derive(Serialize)]
struct Unsupported<'a> {
    supported: [&'static str; 2],
    requested: &'a str,
}

fn answered<'a>(answer: Answer) -> Message<'a> {
    Message::Answered(answer)
}

/// Decode an `Mcp-Name` value: the specification's base64 form
/// (`=?base64?…?=`, standard alphabet) or the plain value.
fn decode_header(value: &[u8]) -> Option<Cow<'_, [u8]>> {
    match value
        .strip_prefix(b"=?base64?")
        .and_then(|rest| rest.strip_suffix(b"?="))
    {
        Some(encoded) => base64_decode(encoded).map(Cow::Owned),
        None => Some(Cow::Borrowed(value)),
    }
}

/// Standard-alphabet base64, padded or not; `None` for anything else.
fn base64_decode(encoded: &[u8]) -> Option<Vec<u8>> {
    let sextet = |byte: u8| -> Option<u32> {
        Some(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let trimmed = encoded
        .strip_suffix(b"==")
        .or_else(|| encoded.strip_suffix(b"="))
        .unwrap_or(encoded);
    if trimmed.len() % 4 == 1
        || (trimmed.len() != encoded.len() && !encoded.len().is_multiple_of(4))
    {
        return None;
    }
    let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
    for chunk in trimmed.chunks(4) {
        let mut bits = 0u32;
        for (i, byte) in chunk.iter().enumerate() {
            bits |= sextet(*byte)? << (18 - 6 * i);
        }
        let bytes = bits.to_be_bytes();
        out.extend_from_slice(&bytes[1..chunk.len()]);
    }
    Some(out)
}

/// A JSON-RPC error, its id echoed where one was read and omitted otherwise:
/// both revisions' `RequestId` excludes `null`.
fn error(status: u16, id: Option<&str>, code: i32, message: &str, data: Option<&str>) -> Answer {
    let mut body = Vec::with_capacity(128);
    body.extend_from_slice(br#"{"jsonrpc":"2.0","#);
    if let Some(id) = id {
        body.extend_from_slice(br#""id":"#);
        body.extend_from_slice(id.as_bytes());
        body.push(b',');
    }
    body.extend_from_slice(br#""error":{"code":"#);
    body.extend_from_slice(code.to_string().as_bytes());
    body.extend_from_slice(br#","message":"#);
    serde_json::to_writer(&mut body, message).expect("a message serializes");
    if let Some(data) = data {
        body.extend_from_slice(br#","data":"#);
        body.extend_from_slice(data.as_bytes());
    }
    body.extend_from_slice(b"}}");
    Answer {
        status,
        body,
        error_code: Some(code),
        wire_error: None,
    }
}

/// A 200 answer carrying `result`, which is complete as it stands.
fn response(id: &str, result: &[u8]) -> Answer {
    let mut body = Vec::with_capacity(result.len() + id.len() + 32);
    body.extend_from_slice(br#"{"jsonrpc":"2.0","id":"#);
    body.extend_from_slice(id.as_bytes());
    body.extend_from_slice(br#","result":"#);
    body.extend_from_slice(result);
    body.push(b'}');
    Answer {
        status: 200,
        body,
        error_code: None,
        wire_error: None,
    }
}

/// A 200 answer carrying `fields`, an object's members without their braces
/// or an empty object; under 2026-07-28 it gains `resultType` and the server
/// identity, as that revision asks of every result.
fn result(id: &str, revision: Revision, server_info: &str, fields: &[u8]) -> Answer {
    let mut object = Vec::with_capacity(fields.len() + server_info.len() + 96);
    let inner = fields
        .strip_prefix(b"{")
        .and_then(|rest| rest.strip_suffix(b"}"))
        .expect("result fields are an object");
    object.push(b'{');
    object.extend_from_slice(inner);
    if revision == Revision::Modern {
        if !inner.is_empty() {
            object.push(b',');
        }
        modern_fields(&mut object, server_info);
    }
    object.push(b'}');
    response(id, &object)
}

/// The members every 2026-07-28 result carries.
fn modern_fields(out: &mut Vec<u8>, server_info: &str) {
    out.extend_from_slice(
        br#""resultType":"complete","_meta":{"io.modelcontextprotocol/serverInfo":"#,
    );
    out.extend_from_slice(server_info.as_bytes());
    out.push(b'}');
}

fn initialize_result(server_info: &str) -> Vec<u8> {
    // The requested version is answered when it is 2025-11-25, and otherwise
    // countered with it: either way the answer names 2025-11-25.
    format!(
        r#"{{"protocolVersion":"{LEGACY_VERSION}","capabilities":{{"tools":{{}}}},"serverInfo":{server_info}}}"#
    )
    .into_bytes()
}

fn discover_result() -> Vec<u8> {
    format!(
        r#"{{"supportedVersions":["{MODERN_VERSION}","{LEGACY_VERSION}"],"capabilities":{{"tools":{{}}}},"ttlMs":{TTL_MS},"cacheScope":"public"}}"#
    )
    .into_bytes()
}

/// A failure carried in band: the envelope and its class beside it, since a
/// tool outcome is always status 200.
#[derive(Serialize)]
struct Failure<'a> {
    error: &'a ErrorEnvelope,
    class: FaultClass,
}

impl Reply {
    /// The revision this reply is composed under.
    pub fn revision(&self) -> Revision {
        self.revision
    }

    /// A report's result: the report verbatim as `structuredContent`, and the
    /// same bytes escaped once as the text block. Every report is a JSON
    /// object.
    pub fn report(&self, report: &[u8]) -> Answer {
        self.tool_result(report, None)
    }

    /// A wire failure's `isError` result: the envelope and its fault class.
    pub fn failure(&self, envelope: &ErrorEnvelope) -> Answer {
        let structured = serde_json::to_vec(&Failure {
            error: envelope,
            class: envelope.fault_class(),
        })
        .expect("a failure serializes");
        self.tool_result(&structured, Some(envelope.code().clone()))
    }

    /// A caller supplied a mode the tool name already fixes.
    pub fn mode_given(&self, verb: Verb) -> Answer {
        let preview = tool_name(verb, true);
        self.bad_request(format!(
            "`mode` is set by the tool's name: call `{verb}` to apply or \
             `{preview}` to preview, without a `mode` argument; nothing was dispatched"
        ))
    }

    /// The route could not read the arguments it needs before dispatch.
    pub fn bad_arguments(&self, verb: Verb, error: &str) -> Answer {
        self.bad_request(format!(
            "the arguments are not `{verb}`'s params: {error}; nothing was dispatched"
        ))
    }

    fn bad_request(&self, message: String) -> Answer {
        self.failure(&ErrorEnvelope::new(message, ErrorDetail::bad_request()))
    }

    fn tool_result(&self, structured: &[u8], wire_error: Option<ReasonCode>) -> Answer {
        debug_assert!(structured.first() == Some(&b'{'), "a report is an object");
        // A report is the JSON serde wrote, so its bytes are UTF-8; the lossy
        // reading is never taken.
        let text = String::from_utf8_lossy(structured);
        let mut body = Vec::with_capacity(structured.len() * 2 + self.server_info.len() + 160);
        body.extend_from_slice(br#"{"jsonrpc":"2.0","id":"#);
        body.extend_from_slice(self.id.as_bytes());
        body.extend_from_slice(br#","result":{"content":[{"type":"text","text":"#);
        serde_json::to_writer(&mut body, text.as_ref()).expect("a string serializes");
        body.extend_from_slice(br#"}],"structuredContent":"#);
        body.extend_from_slice(structured);
        if wire_error.is_some() {
            body.extend_from_slice(br#","isError":true"#);
        }
        if self.revision == Revision::Modern {
            body.push(b',');
            modern_fields(&mut body, &self.server_info);
        }
        body.extend_from_slice(b"}}");
        Answer {
            status: 200,
            body,
            error_code: None,
            wire_error,
        }
    }
}

/// A call's wire body: its arguments, with the tool's mode spliced in for a
/// mode tool. A `mode` key the caller gave is refused, never overridden, in
/// either direction.
pub fn wire_body(arguments: &[u8], mode: Option<ApplyMode>) -> Result<Cow<'_, [u8]>, ModeGiven> {
    Ok(ModeSplice::check(arguments, mode)?.apply(arguments))
}

impl ModeSplice {
    fn check(arguments: &[u8], mode: Option<ApplyMode>) -> Result<Self, ModeGiven> {
        let Some(mode) = mode else {
            return Ok(Self(None));
        };
        #[derive(Deserialize)]
        struct ModeKey<'a> {
            #[serde(default, borrow, deserialize_with = "present")]
            mode: Option<&'a RawValue>,
        }
        // Only the one key is read. Escaped spellings of `mode` also match,
        // and a duplicate `mode` also refuses the call.
        match serde_json::from_slice::<ModeKey<'_>>(arguments) {
            Ok(ModeKey { mode: None }) => {}
            _ => return Err(ModeGiven),
        }
        // A mode tool's arguments must be an object: its opening brace is
        // what `apply` finds.
        if !arguments.contains(&b'{') {
            return Err(ModeGiven);
        }
        Ok(Self(Some(mode)))
    }

    /// Splice the mode into the same argument bytes that were checked.
    pub fn apply(self, arguments: &[u8]) -> Cow<'_, [u8]> {
        let Some(mode) = self.0 else {
            return Cow::Borrowed(arguments);
        };
        let Some(open) = arguments.iter().position(|byte| *byte == b'{') else {
            return Cow::Borrowed(arguments);
        };
        let rest = &arguments[open + 1..];
        let empty = rest
            .iter()
            .find(|byte| !byte.is_ascii_whitespace())
            .is_some_and(|byte| *byte == b'}');
        let spelled: &[u8] = match mode {
            ApplyMode::Apply => br#""mode":"apply""#,
            ApplyMode::Preview => br#""mode":"preview""#,
        };
        let mut body = Vec::with_capacity(arguments.len() + spelled.len() + 1);
        body.push(b'{');
        body.extend_from_slice(spelled);
        if !empty {
            body.push(b',');
        }
        body.extend_from_slice(rest);
        Cow::Owned(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_names_decode_in_the_standard_alphabet() {
        assert_eq!(
            decode_header(b"=?base64?SGVsbG8sIOS4lueVjA==?=").as_deref(),
            Some("Hello, 世界".as_bytes())
        );
        assert_eq!(
            decode_header(b"=?base64?IHBhZGRlZCA=?=").as_deref(),
            Some(" padded ".as_bytes())
        );
        assert_eq!(
            decode_header(b"=?base64?PT9iYXNlNjQ/bGl0ZXJhbD89?=").as_deref(),
            Some("=?base64?literal?=".as_bytes())
        );
        assert_eq!(decode_header(b"find").as_deref(), Some(b"find".as_slice()));
        assert_eq!(decode_header(b"=?base64?Z*==?="), None);
        assert_eq!(decode_header(b"=?base64?Z?="), None);
    }

    #[test]
    fn a_request_id_is_a_string_or_an_integer() {
        for id in [r#""a""#, "1", "-4", "12345678901234567890"] {
            assert!(is_request_id(id), "{id}");
        }
        for id in ["null", "1.5", "1e3", "true", "{}", "[]"] {
            assert!(!is_request_id(id), "{id}");
        }
    }
}
