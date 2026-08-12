//! Frontmatter: reading the leading `---` block, locating its fields, and
//! emitting bytes that provably read back.

pub(crate) mod extract;
pub(crate) mod fields;
pub(crate) mod render;

pub use extract::FRONTMATTER_MAX_BYTES;
pub use fields::{Field, ValueStyle};
pub use render::{RenderError, ScalarContext, render_document};
