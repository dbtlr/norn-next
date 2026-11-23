//! Pure MCP translation over the native wire vocabulary: the tool
//! definitions derived from it ([`ToolList`]), and every message `/mcp`
//! reads and answers ([`message`]). No transport, no async and no I/O.
use norn_wire::{ApplyMode, Verb, VerbEffect};
use serde::Serialize;
use serde_json::{Value, json};
use std::fmt;
use std::sync::Arc;

pub mod message;

pub use message::{
    Answer, Call, Headers, LEGACY_VERSION, MODERN_VERSION, Message, ModeGiven, ModeSplice, Reply,
    Revision, ToolRoute, handle, wire_body,
};

/// A params schema has assertions that cannot safely be removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaError(pub Verb);
impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} has an unsupported root schema constraint", self.0)
    }
}
impl std::error::Error for SchemaError {}

/// Hints describe effects, not the pool a call runs on.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotations {
    pub read_only_hint: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    pub open_world_hint: bool,
}

/// One advertised tool and the native operation it denotes.
#[derive(Debug, Serialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    pub annotations: Annotations,
    #[serde(rename = "_meta")]
    pub meta: Value,
    #[serde(skip)]
    pub verb: Verb,
    #[serde(skip)]
    pub mode: Option<ApplyMode>,
}

/// Tool definitions derived from the served wire vocabulary at startup.
/// Preparing result bytes does not derive these schemas again.
#[derive(Debug)]
pub struct ToolList {
    pub tools: Vec<Tool>,
}

/// What the serving process keeps from one derivation: the two encoded
/// `tools/list` results, the name and native operation of each tool a call is
/// resolved by, and the server identity every 2026-07-28 result carries.
#[derive(Debug)]
pub struct PreparedToolLists {
    pub legacy_body: Vec<u8>,
    pub modern_body: Vec<u8>,
    /// Each served tool's name and what it runs, in list order.
    pub tools: Vec<ToolRoute>,
    /// The server identity, `{"name":"norn","version":<build>}`, as JSON.
    pub server_info: Arc<str>,
}

impl ToolList {
    /// Derive only served verbs, retaining vocabulary order regardless of input order.
    pub fn derive(served: impl IntoIterator<Item = Verb>) -> Result<Self, SchemaError> {
        let served: std::collections::BTreeSet<_> =
            served.into_iter().map(|verb| verb.as_str()).collect();
        let mut tools = Vec::new();
        for verb in Verb::ALL {
            if !served.contains(verb.as_str()) {
                continue;
            }
            let schema = verb.params_schema().to_value();
            let (input_schema, removed) = input_schema(verb, schema)?;
            for preview in [false, true] {
                if preview && !verb.takes_mode() {
                    continue;
                }
                let (group, destructive) = if preview {
                    ("read", None)
                } else {
                    match verb.effect() {
                        VerbEffect::Reads => ("read", None),
                        VerbEffect::Writes { destructive } => ("write", Some(destructive)),
                        VerbEffect::Administers { destructive } => ("admin", Some(destructive)),
                    }
                };
                let mut description = verb.summary().to_owned();
                if removed {
                    description.push_str(match verb {
                        Verb::New => " Use either `path` with `content` only, `as` with optional `variables`, `fields` and `body`, or inbox `fields` and `body` without `path`, `as`, `content` or `variables`.",
                        Verb::Set => " Give exactly one of `path` or `where`.",
                        Verb::Delete => " Never give `rewrite_to` together with `allow_broken_links: true`.",
                        _ => return Err(SchemaError(verb)),
                    });
                }
                if preview {
                    description.push_str(" Previews only: writes nothing, and answers with the plan and its forecast.");
                }
                tools.push(Tool {
                    name: tool_name(verb, preview),
                    description,
                    input_schema: input_schema.clone(),
                    annotations: Annotations {
                        read_only_hint: group == "read",
                        destructive_hint: destructive,
                        open_world_hint: false,
                    },
                    meta: json!({"modgud/groups": [group]}),
                    verb,
                    mode: verb.takes_mode().then_some(if preview {
                        ApplyMode::Preview
                    } else {
                        ApplyMode::Apply
                    }),
                });
            }
        }
        Ok(Self { tools })
    }

    /// Encode the revision-specific results using the hello's build identity.
    /// Both bodies serialize the borrowed tools directly: building a JSON tree
    /// of the list first would copy every schema once per revision.
    pub fn prepare(&self, build: &str) -> PreparedToolLists {
        #[derive(Serialize)]
        struct LegacyResult<'a> {
            tools: &'a [Tool],
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct ModernResult<'a> {
            tools: &'a [Tool],
            result_type: &'static str,
            ttl_ms: u64,
            cache_scope: &'static str,
            #[serde(rename = "_meta")]
            meta: Value,
        }
        let server_info = json!({"name": "norn", "version": build});
        let legacy_body =
            serde_json::to_vec(&LegacyResult { tools: &self.tools }).expect("tools serialize");
        let modern_body = serde_json::to_vec(&ModernResult {
            tools: &self.tools,
            result_type: "complete",
            ttl_ms: message::TTL_MS,
            cache_scope: "public",
            meta: json!({"io.modelcontextprotocol/serverInfo": server_info}),
        })
        .expect("tools serialize");
        PreparedToolLists {
            legacy_body,
            modern_body,
            tools: self
                .tools
                .iter()
                .map(|tool| ToolRoute {
                    name: tool.name.clone(),
                    verb: tool.verb,
                    mode: tool.mode,
                })
                .collect(),
            server_info: server_info.to_string().into(),
        }
    }
}

fn tool_name(verb: Verb, preview: bool) -> String {
    format!("{}{}", verb.as_str(), if preview { "_preview" } else { "" })
}

/// Remove only presence/constant assertions over properties already at the root.
pub fn input_schema(verb: Verb, mut schema: Value) -> Result<(Value, bool), SchemaError> {
    let root = schema.as_object_mut().ok_or(SchemaError(verb))?;
    if verb.takes_mode() {
        if let Some(properties) = root.get_mut("properties").and_then(Value::as_object_mut) {
            properties.remove("mode");
        }
        if let Some(required) = root.get_mut("required").and_then(Value::as_array_mut) {
            required.retain(|key| key != "mode");
        }
    }
    root.remove("$schema");
    let properties = root.get("properties").cloned().unwrap_or(json!({}));
    let mut removed = false;
    for key in ["oneOf", "anyOf", "allOf", "not"] {
        if let Some(assertion) = root.remove(key) {
            let valid = if key == "not" {
                presence(&assertion, &properties)
            } else {
                assertion.as_array().is_some_and(|branches| {
                    !branches.is_empty()
                        && branches.iter().all(|branch| presence(branch, &properties))
                })
            };
            if !valid {
                return Err(SchemaError(verb));
            }
            removed = true;
        }
    }
    root.insert("type".into(), json!("object"));
    Ok((schema, removed))
}

fn presence(value: &Value, properties: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        object.iter().all(|(key, value)| match key.as_str() {
            "required" => value.as_array().is_some_and(|keys| {
                keys.iter().all(|key| {
                    key.as_str()
                        .is_some_and(|key| properties.get(key).is_some())
                })
            }),
            "not" => presence(value, properties),
            "anyOf" => value.as_array().is_some_and(|branches| {
                !branches.is_empty() && branches.iter().all(|branch| presence(branch, properties))
            }),
            "properties" => value.as_object().is_some_and(|entries| {
                entries.iter().all(|(key, assertion)| {
                    properties.get(key).is_some()
                        && assertion.as_object().is_some_and(|assertion| {
                            assertion.len() == 1 && assertion.contains_key("const")
                        })
                })
            }),
            _ => false,
        })
    })
}

impl Tool {
    /// The native params body for `arguments`, the raw JSON object a caller
    /// sent: the bytes as they arrived, with the tool's mode spliced in for a
    /// mode tool through [`wire_body`]. The arguments are never parsed and
    /// re-serialized, so key order and a duplicate key reach the wire
    /// parser exactly as sent. The tool name owns mode; a caller cannot
    /// override it.
    pub fn wire_body(&self, arguments: &[u8]) -> Result<Vec<u8>, &'static str> {
        let object = serde_json::from_slice::<&serde_json::value::RawValue>(arguments)
            .map_err(|_| "arguments must be JSON")?;
        if !object.get().starts_with('{') {
            return Err("arguments must be an object");
        }
        wire_body(arguments, self.mode)
            .map(std::borrow::Cow::into_owned)
            .map_err(|ModeGiven| "mode is fixed by the tool name")
    }
}
