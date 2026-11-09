//! The authored baselines the walker's memory suite asserts against.
//!
//! **Every value here is authored, and it moves only by a reviewed edit.** The
//! file is the trend's whole memory: a measurement that drifts fails rather
//! than quietly redefining what normal is. A number moves when somebody
//! changes it in a diff, with the grounds beside it. Each constant is
//! registered in the certification ledger's exit-bar roll.

// Three native macos-arm64 debug readings after the descriptor-safe fix on
// 2026-08-01 put `ambiguous` at 2.81-2.89 MiB and `realistic` at 3.22-3.27 MiB,
// ratios of 1.13-1.16. These macOS readings predate NORN-443.
// The ceiling is more than twice the observed top; the
// ratio below is the sharp instrument against retaining the realistic tree's
// roughly 4.3 MiB of clutter in addition to the executable's resident floor.
#[cfg(target_os = "macos")]
pub const REALISTIC_PEAK_RSS_CEILING_BYTES: u64 = 8 * 1024 * 1024;

// The existing fixture generator's ubuntu-latest x86_64-glibc baseline is
// 3.50-3.62 MiB versus 3.7-4.2 MiB on macos-arm64. Before this walker lane's
// first hosted reading, Linux therefore used the same conservative bar;
// subsequent changes move it only with reviewed measurement evidence. This is
// the same 8 MiB conservative ceiling already used for that generator lane.
// On ubuntu-latest x86_64-glibc, full memory invariant runs 41323116407,
// 41324737076 and 41329840687 at PR #423 head 5dd80988b read ambiguous at
// 3.78, 3.66 and 3.66 MiB, realistic at 3.86, 3.88 and 3.84 MiB, and ratios
// of 1.02, 1.06 and 1.05. These are the child's own peaks (NORN-443).
// The ceiling stays at 8 MiB, the smallest whole MiB above twice the top
// reading (2 * 3.88 = 7.76 MiB).
#[cfg(target_os = "linux")]
pub const REALISTIC_PEAK_RSS_CEILING_BYTES: u64 = 8 * 1024 * 1024;

// Other Unix targets have no hosted reading yet; use the same conservative
// ceiling until one earns a target-specific measured bar.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub const REALISTIC_PEAK_RSS_CEILING_BYTES: u64 = 8 * 1024 * 1024;

// A retained-tree cost grows by several MiB between these profiles. A 2x bar
// tolerates process-accounting noise around the executable's fixed floor but
// still rejects growth proportional to the generated tree. The hosted
// 1.02-1.06x readings above leave this rule unchanged.
pub const REALISTIC_TO_AMBIGUOUS_PEAK_RSS_PER_MILLE: u64 = 2_000;
