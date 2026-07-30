//! Pulling the leading `---` … `---` block out of a document.
//!
//! Extraction is forgiving: an absent, unclosed or malformed block yields no
//! value plus a diagnostic, never an error return, and the body is still
//! reported. Offsets are absolute in the original content, byte-order mark
//! included, so a splice computed from them lands where it was measured.

use std::ops::Range;

use crate::diagnostic::Diagnostic;
use crate::value::{StripReport, Value, from_yaml};

/// The byte length of a UTF-8 byte-order mark.
pub(crate) const BOM: &str = "\u{feff}";

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
            Err(error) => {
                diagnostics.push(
                    Diagnostic::warning(
                        "frontmatter-parse-failed",
                        "frontmatter could not be parsed",
                    )
                    .with_detail(error),
                );
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
        "frontmatter-unclosed",
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

/// Parse the YAML between the delimiters, expanding merge keys.
///
/// `<<` is a merge directive rather than a field: the mapping it names is
/// folded into the one holding it, and the vault value model — which addresses
/// fields by string key — has no shape for the directive itself. Expansion
/// happens here, before a value exists, so the model holds the merged mapping
/// and `<<` never becomes a field. A `<<` that names no mapping is not a merge
/// and cannot be expanded, so the block is refused rather than read with a
/// directive silently carried as a field.
pub(crate) fn parse_block(yaml: &str) -> Result<serde_yaml::Value, String> {
    let mut parsed: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|error| error.to_string())?;
    parsed.apply_merge().map_err(|error| error.to_string())?;
    Ok(parsed)
}
