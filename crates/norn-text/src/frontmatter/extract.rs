//! Pulling the leading `---` … `---` block out of a document.
//!
//! Extraction is forgiving: an absent, unclosed, malformed or oversized block
//! yields no value plus a diagnostic, never an error return, and the body is
//! still reported. Offsets are absolute in the original content, byte-order
//! mark included, so a splice computed from them lands where it was measured.
//!
//! A block is read only up to [`FRONTMATTER_MAX_BYTES`], which is what bounds
//! what one document costs to read.

use std::ops::Range;

use crate::diagnostic::{Diagnostic, DiagnosticCode};
use crate::value::{StripReport, Value, from_yaml};

/// The byte length of a UTF-8 byte-order mark.
pub(crate) const BOM: &str = "\u{feff}";

/// The largest frontmatter block this crate reads, in bytes, counted between
/// the delimiters.
///
/// The bound is what holds the cost of reading one document. The YAML scanner
/// behind this seam is superlinear in the length of a block — a block that
/// nests flow collections costs time quadratic in its own length, so a document
/// that looks ordinary can carry a block worth seconds of CPU — and length is
/// the input that decides how far that goes. Sixteen kibibytes is two orders of
/// magnitude past the largest block a hand-written vault document carries,
/// where a block is a few hundred bytes of title, dates, status and tags, so no
/// authored frontmatter comes near it and the shapes that do are not
/// frontmatter anybody wrote.
///
/// A block past the bound is refused rather than parsed or truncated: the
/// document carries no frontmatter value and a
/// [`FrontmatterTooLarge`](crate::DiagnosticCode::FrontmatterTooLarge) note
/// saying so, exactly as a block that is not well-formed YAML does. Reading
/// stays forgiving here; what a consumer does with a document whose block was
/// refused is the consumer's contract.
pub const FRONTMATTER_MAX_BYTES: usize = 16 * 1024;

pub(crate) struct Extraction<'a> {
    /// The parsed block, or `None` when there is none or it is malformed.
    pub(crate) value: Option<Value>,
    /// The byte range of the YAML between the delimiters. Present even when
    /// the block did not parse; absent when there is no closed block at all.
    pub(crate) range: Option<Range<usize>>,
    pub(crate) body: &'a str,
    pub(crate) body_start: usize,
    /// True when the document opens with a byte-order mark.
    pub(crate) byte_order_mark: bool,
    pub(crate) strip: StripReport,
}

/// Extract the leading frontmatter block from `content`.
pub(crate) fn extract<'a>(content: &'a str, diagnostics: &mut Vec<Diagnostic>) -> Extraction<'a> {
    // A byte-order mark ahead of the fence must not hide the block. Only
    // recognition steps past it: the byte stays in the document and every
    // offset below is content-absolute, so an edit splices after it and leaves
    // it untouched.
    let byte_order_mark = content.starts_with(BOM);
    let after_bom = if byte_order_mark {
        &content[BOM.len()..]
    } else {
        content
    };

    let absent = || Extraction {
        value: None,
        range: None,
        body: content,
        body_start: 0,
        byte_order_mark,
        strip: StripReport::default(),
    };

    let Some(after_open) = strip_opening_fence(after_bom) else {
        return absent();
    };

    let mut offset = content.len() - after_open.len();
    let yaml_start = offset;
    for line in after_open.split_inclusive('\n') {
        if !is_fence(line) {
            offset += line.len();
            continue;
        }

        let range = yaml_start..offset;
        let body_start = offset + line.len();
        let body = &content[body_start..];
        let mut strip = StripReport::default();
        let value = match parse_block(&content[range.clone()]) {
            Ok(parsed) => Some(from_yaml(
                parsed,
                &mut String::new(),
                diagnostics,
                &mut strip,
            )),
            Err(refusal) => {
                diagnostics.push(refusal.into_diagnostic());
                None
            }
        };
        return Extraction {
            value,
            range: Some(range),
            body,
            body_start,
            byte_order_mark,
            strip,
        };
    }

    diagnostics.push(Diagnostic::warning(
        DiagnosticCode::FrontmatterUnclosed,
        "frontmatter opening delimiter has no closing delimiter",
    ));
    absent()
}

/// The bytes after an opening `---` fence, or `None` when `text` does not open
/// one.
///
/// A fence is `---` plus any spaces or tabs, then a line break. The trailing
/// whitespace is accepted for the reason the closing fence accepts it — the
/// two are one delimiter written twice, and an editor that trims neither
/// writes both. Refusing it is not a smaller contract but a corrupting one: a
/// document whose block goes unrecognized has its fields written into a
/// *second* block synthesized above the first, and the re-read then finds the
/// synthesized one and approves.
fn strip_opening_fence(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---")?.trim_start_matches([' ', '\t']);
    rest.strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))
}

/// Whether `line` is a `---` delimiter: the three dashes, then nothing but
/// spaces, tabs and the line terminator.
fn is_fence(line: &str) -> bool {
    let text = line.trim_end_matches(['\r', '\n']);
    text.strip_prefix("---")
        .is_some_and(|rest| rest.bytes().all(|byte| byte == b' ' || byte == b'\t'))
}

/// The key a merge directive is written under.
pub(crate) const MERGE_KEY: &str = "<<";

/// Why a frontmatter block yields no value.
///
/// The two refusals are told apart because they are different defects in the
/// document: one block is written in a YAML this crate cannot read, and the
/// other is larger than the block it reads at all.
#[derive(Debug)]
pub(crate) enum BlockRefusal {
    /// The block is longer than [`FRONTMATTER_MAX_BYTES`], carrying its own
    /// length so the note can state how far past the bound it is.
    TooLarge { bytes: usize },
    /// The block is not well-formed YAML, or carries a merge directive that
    /// names no mapping.
    Unreadable { problem: String },
}

impl BlockRefusal {
    /// The note this refusal is filed as.
    pub(crate) fn into_diagnostic(self) -> Diagnostic {
        match self {
            BlockRefusal::TooLarge { bytes } => Diagnostic::warning(
                DiagnosticCode::FrontmatterTooLarge,
                "frontmatter is larger than the block that is read",
            )
            .with_detail(format!(
                "the block is {bytes} bytes and the bound is {FRONTMATTER_MAX_BYTES}"
            )),
            BlockRefusal::Unreadable { problem } => Diagnostic::warning(
                DiagnosticCode::FrontmatterParseFailed,
                "frontmatter could not be parsed",
            )
            .with_detail(problem),
        }
    }
}

/// Parse the YAML between the delimiters, expanding merge keys.
///
/// This is the one place document text reaches a YAML parser, so it is where
/// [`FRONTMATTER_MAX_BYTES`] is enforced: a block past the bound is refused
/// here and no parser sees it, which is what keeps the cost of reading one
/// document bounded by the bound rather than by what the block is shaped like.
///
/// `<<` is a merge directive rather than a field: the mapping it names is
/// folded into the one holding it, and the value model — which addresses
/// fields by string key — has no shape for the directive itself. Expansion
/// happens here, before a value exists, so the model holds the merged mapping
/// and `<<` never becomes a field. A `<<` that names no mapping is not a merge
/// and cannot be expanded, so the block is refused rather than read with a
/// directive silently carried as a field.
pub(crate) fn parse_block(yaml: &str) -> Result<serde_yaml::Value, BlockRefusal> {
    if yaml.len() > FRONTMATTER_MAX_BYTES {
        return Err(BlockRefusal::TooLarge { bytes: yaml.len() });
    }
    let mut parsed: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|error| BlockRefusal::Unreadable {
            problem: error.to_string(),
        })?;
    expand_merges(&mut parsed).map_err(|problem| BlockRefusal::Unreadable { problem })?;
    Ok(parsed)
}

/// Fold every `<<` directive into the mapping holding it, in place.
///
/// **Key order is part of the answer.** An explicit key keeps the position the
/// document wrote it in — the directive's own line vacates, and nothing else
/// moves — and a key only the merge contributes is appended after every
/// explicit one, in the order the merged mapping wrote it. An explicit key
/// wins over a merged one of the same name, and among several sources the
/// first to name a key wins, which is what YAML says a merge means.
///
/// This is why `serde_yaml::Value::apply_merge` is not what runs here: it
/// removes `<<` by swapping the mapping's *last* entry into the vacated slot,
/// which permutes document order. Order is a contract this crate keeps —
/// fields are never reordered, and a re-read that finds them moved is an edit
/// this crate refuses to prove.
///
/// The walk expands a mapping's children before the mapping itself, so a
/// merged source that carries a `<<` of its own is already expanded when it is
/// folded in and no directive survives anywhere in the value.
fn expand_merges(value: &mut serde_yaml::Value) -> Result<(), String> {
    match value {
        serde_yaml::Value::Sequence(items) => items.iter_mut().try_for_each(expand_merges),
        serde_yaml::Value::Tagged(tagged) => expand_merges(&mut tagged.value),
        serde_yaml::Value::Mapping(mapping) => {
            for (_, child) in mapping.iter_mut() {
                expand_merges(child)?;
            }
            let Some(directive) = mapping.shift_remove(MERGE_KEY) else {
                return Ok(());
            };
            for source in merge_sources(directive)? {
                for (key, value) in source {
                    mapping.entry(key).or_insert(value);
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The mappings a `<<` names: one, or a sequence of them. Anything else is not
/// a merge and has no expansion, so it refuses the block.
fn merge_sources(directive: serde_yaml::Value) -> Result<Vec<serde_yaml::Mapping>, String> {
    match directive {
        serde_yaml::Value::Mapping(mapping) => Ok(vec![mapping]),
        serde_yaml::Value::Sequence(items) => items
            .into_iter()
            .map(|item| match item {
                serde_yaml::Value::Mapping(mapping) => Ok(mapping),
                other => Err(not_a_merge_source(&other)),
            })
            .collect(),
        other => Err(not_a_merge_source(&other)),
    }
}

fn not_a_merge_source(value: &serde_yaml::Value) -> String {
    let kind = match value {
        serde_yaml::Value::Null => "null",
        serde_yaml::Value::Bool(_) => "a bool",
        serde_yaml::Value::Number(_) => "a number",
        serde_yaml::Value::String(_) => "a string",
        serde_yaml::Value::Sequence(_) => "a sequence",
        serde_yaml::Value::Mapping(_) => "a mapping",
        serde_yaml::Value::Tagged(_) => "a tagged value",
    };
    format!("a merge key must name a mapping or a sequence of mappings, but it names {kind}")
}
