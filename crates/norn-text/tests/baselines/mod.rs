//! The authored baselines this crate's measurement suite asserts against.
//!
//! **Every value here is authored, and it moves only by a reviewed edit.** The
//! file is the trend's whole memory: a measurement that drifts fails rather
//! than quietly redefining what normal is. There is no history to fetch and no
//! store to consult — a number moves when somebody changes it in a diff, with
//! the grounds beside it.
//!
//! What binds mechanically is the comparison: a reading past the value here
//! fails. The direction the values travel is review-held — nothing forbids
//! raising one, and a reviewer reading the diff is what asks for the claim that
//! the subject now costs more. Lowering one needs no new argument.
//!
//! # Where the readings come from
//!
//! Every band below is over a flat mapping of a stated key count — two of them
//! reading it with `Document::parse` and one deriving every field text out of a
//! block already parsed — best of five timed samples after three warm ones.
//! Repeated local
//! readings cover **macos-arm64**, in both the pinned toolchain's unoptimized
//! build — which is what the lane runs — and its optimized one. The lane's
//! first hosted run on `ubuntu-latest` x86_64-glibc is what produces the hosted
//! readings; until it does, the values here are the local ones with headroom
//! over them.
//!
//! The ratio is the value worth reading. A ceiling passes anything that fits
//! under it, and the defect this suite exists to catch is a *shape*: a cost
//! that grows faster than the key count does. The ceiling beside it is a sanity
//! bar, and the wall-clock reading recorded in the job summary is what a person
//! looks at.
#![allow(dead_code)]

use std::time::Duration;

/// How much of four quarter-scale reads one whole-block read may cost, per
/// mille.
///
/// **The linearity invariant.** The subject is a flat mapping of distinct keys,
/// at the most keys the block bound admits and at a quarter of them. The two
/// arrangements carry the same keys — one block of `n`, or four blocks of `n /
/// 4` — so a read linear in key count costs the same either way, and the split
/// side pays three extra fixed parse overheads on top of it. The reading is
/// therefore below 1.0 whenever the shape holds.
///
/// Observed over 3 runs per build: **0.95–1.05 optimized and 0.96–1.00
/// unoptimized on macos-arm64**, against 2730 keys whole and 682 split.
///
/// The bar is 1.5, which leaves close to half again over the worse reading.
/// What it forbids is a read whose cost is quadratic in key count: resolving
/// each scanned key line by scanning the parsed mapping for it reads
/// **3.19–3.27 optimized and 2.84–2.87 unoptimized** at the same pair, better
/// than 1.8x past this bar on both.
pub const WHOLE_AGAINST_SPLIT_PER_MILLE: u64 = 1_500;

/// How much of four quarter-scale derives one whole-block derive may cost, per
/// mille.
///
/// **The linearity invariant over the derive walk.** `Document::field_texts`
/// resolves every field's key against the parsed mapping a second time, on the
/// path a caller reading tags or wikilinks out of a block goes through. The
/// arrangement is the one above — one block of `n` fields against four of `n /
/// 4` — with a one-character string under every key so each field yields a text,
/// at 2048 keys whole and 512 split.
///
/// The parse is outside this clock. It is linear on both sides and it is the
/// larger share of an ordinary read, and a reading that includes it measures
/// **1.44 unoptimized and 1.58 optimized** against the same walk that measures
/// 2.94 and 2.78 alone: the dilution is enough to carry a quadratic walk under
/// this bar.
///
/// Observed over 3 runs per build: **0.94–1.03 optimized and 0.99–1.04
/// unoptimized on macos-arm64**.
///
/// The bar is 1.5, the same as the read above and for the same reason. What it
/// forbids is a walk whose cost is quadratic in field count: resolving each
/// field by scanning the parsed mapping for it reads **2.78–3.95 optimized and
/// 2.94–4.26 unoptimized** at the same pair.
pub const DERIVE_WHOLE_AGAINST_SPLIT_PER_MILLE: u64 = 1_500;

/// Wall clock a full-bound flat mapping's parse must finish inside.
///
/// A sanity ceiling, not a bar on speed: the parse of a 2730-key block bills
/// **1.21–1.27 ms optimized and 12.82–12.90 ms unoptimized on macos-arm64**,
/// over 3 runs per build, and this is roughly forty times the unoptimized
/// reading. A hosted runner competing for a core is slower still, in ways no
/// regression is, so anything tight enough to catch a regression here would
/// fail for reasons that are not about this repository — which is what
/// [`WHOLE_AGAINST_SPLIT_PER_MILLE`] is for, and the quadratic read this suite
/// exists to catch passes this ceiling at 77 ms unoptimized. What this forbids
/// is a block at the bound costing a heal worker something it notices.
pub const BOUND_PARSE_CEILING: Duration = Duration::from_millis(500);

/// How a reading is rendered and where it is recorded is the harness's, and
/// every measurement lane in the workspace writes the same table under its run.
/// What is authored per crate is the numbers above, which is what a reviewer
/// reads as one diff.
pub use norn_testkit::readings::{milliseconds, multiple, per_mille, record};
