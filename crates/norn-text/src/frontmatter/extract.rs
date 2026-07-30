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

    let absent = |diagnostics: &mut Vec<Diagnostic>| {
        let _ = diagnostics;
        Extraction {
            value: None,
            range: None,
            body: content,
            body_start: 0,
            byte_order_mark,
            strip: StripReport::default(),
        }
    };

    let Some(after_open) = after_bom
        .strip_prefix("---\n")
        .or_else(|| after_bom.strip_prefix("---\r\n"))
    else {
        return absent(diagnostics);
    };

    let mut offset = content.len() - after_open.len();
    let yaml_start = offset;
    for line in after_open.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) != "---" {
            offset += line.len();
            continue;
        }

        let range = yaml_start..offset;
        let body_start = offset + line.len();
        let body = &content[body_start..];
        let mut strip = StripReport::default();
        let value = match serde_yaml::from_str::<serde_yaml::Value>(&content[range.clone()]) {
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
                    .with_detail(error.to_string()),
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
    absent(diagnostics)
}
