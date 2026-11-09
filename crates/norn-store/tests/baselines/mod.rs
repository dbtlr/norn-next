//! The authored baselines the store's retained-count memory suite asserts
//! against.
//!
//! **Every value here is authored, and it moves only by a reviewed edit.** A
//! number moves when somebody changes it in a diff, with the grounds beside
//! it. Each constant is registered in the certification ledger's exit-bar roll.

// A whole-result Vec at the larger scale retains over 14 MiB more label bytes.
// On ubuntu-latest x86_64-glibc, full memory invariant runs 41323116407,
// 41324737076 and 41329840687 at PR #423 head 5dd80988b read growth of
// -200,704, 102,400 and -20,480 bytes between 512 and 4096 rows.
// No measurement-based lowering rule is authored for either bar.
pub const MEMORY_GROWTH: u64 = 8 * 1024 * 1024;
// The same runs read the child's own peaks (NORN-443), in bytes:
// 512 rows: 14,512,128; 14,282,752; 14,340,096.
// 4096 rows: 14,311,424; 14,385,152; 14,319,616.
pub const MEMORY_CEILING: u64 = 64 * 1024 * 1024;
