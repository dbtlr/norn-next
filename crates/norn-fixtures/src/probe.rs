//! The calibration probe: what a tree's realism statistics are, and the
//! envelope they are expected to land in.
//!
//! # The probe is a shape meter, not a parser
//!
//! [`measure`] reads bytes and counts patterns. It does not interpret them:
//! it never resolves a link, never decides what a frontmatter field means,
//! never honours a code fence or an escape. A heading is a line whose first
//! bytes are one to six `#` followed by a space; a link is an occurrence of
//! `[[`. Document syntax has an owner elsewhere in the workspace, and a
//! second interpretation of it here would be a second grammar. What the probe
//! measures instead is shape — how many files, how big, how deep, how densely
//! cross-referenced — which is exactly what "realistic" is a claim about.
//!
//! Facts that need intent rather than bytes are not measured here. How many
//! of a tree's links dangle, for instance, is reported by the generator in
//! its manifest, because the generator knows which links it made dangle and
//! no byte-level reading can tell.
//!
//! # Where the checked-in parameters come from
//!
//! [`CALIBRATION`] is an **authored envelope**, not a measurement. Each entry
//! states a range a realistic-scale Markdown collection is expected to fall
//! in, and says why in its own `why` field. No entry is derived from any
//! particular collection, and none needs to be: the values are deliberately
//! wide enough to describe collections in general and narrow enough that the
//! failure they exist to catch — a generator drifting back toward uniformly
//! small, uniformly shallow, uniformly linked documents — moves a statistic
//! clean out of range.
//!
//! **Recalibration is a deliberate act.** The probe runs against any
//! directory, a real collection included (`norn-fixtures calibrate <dir>`).
//! Replacing an authored range with a measured one means editing this table,
//! which arrives as a reviewable diff that says what changed and, in the
//! `why` field, on what grounds. Nothing recalibrates itself, and no
//! statistic is read from an environment the repository cannot see.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use crate::tree;

/// Shape statistics for one directory tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VaultStats {
    pub documents: u64,
    pub non_markdown_files: u64,
    pub directories: u64,
    pub max_directory_depth: u64,
    pub markdown_bytes: u64,
    pub document_bytes_p10: u64,
    pub document_bytes_median: u64,
    pub document_bytes_mean: u64,
    pub document_bytes_p90: u64,
    pub document_bytes_max: u64,
    pub heading_lines: u64,
    pub links: u64,
    pub linkless_documents: u64,
    pub ambiguous_stem_classes: u64,
    pub largest_stem_class: u64,
}

impl VaultStats {
    /// Heading lines per mebibyte of Markdown.
    pub fn heading_lines_per_mebibyte(&self) -> u64 {
        ratio(self.heading_lines * 1024 * 1024, self.markdown_bytes)
    }

    /// Links per thousand documents — six links a document reads as 6000.
    pub fn links_per_thousand_documents(&self) -> u64 {
        ratio(self.links * 1000, self.documents)
    }

    /// Documents carrying no links at all, per mille of documents.
    pub fn linkless_documents_per_mille(&self) -> u64 {
        ratio(self.linkless_documents * 1000, self.documents)
    }

    /// Non-Markdown files per mille of all files.
    pub fn non_markdown_files_per_mille(&self) -> u64 {
        ratio(
            self.non_markdown_files * 1000,
            self.documents + self.non_markdown_files,
        )
    }

    /// Documents per thousand directories — fifteen a directory reads as
    /// 15000.
    pub fn documents_per_thousand_directories(&self) -> u64 {
        ratio(self.documents * 1000, self.directories)
    }
}

/// A ratio over an empty tree is zero rather than a panic: the probe reports
/// what it measured, and measuring nothing is a fact, not an error.
fn ratio(numerator: u64, denominator: u64) -> u64 {
    numerator.checked_div(denominator).unwrap_or(0)
}

/// A statistic the calibration envelope constrains.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stat {
    DocumentBytesP10,
    DocumentBytesMedian,
    DocumentBytesMean,
    DocumentBytesP90,
    DocumentBytesMax,
    HeadingLinesPerMebibyte,
    LinksPerThousandDocuments,
    LinklessDocumentsPerMille,
    NonMarkdownFilesPerMille,
    DocumentsPerThousandDirectories,
    MaxDirectoryDepth,
    AmbiguousStemClasses,
    LargestStemClass,
}

impl Stat {
    pub fn name(self) -> &'static str {
        match self {
            Stat::DocumentBytesP10 => "document_bytes_p10",
            Stat::DocumentBytesMedian => "document_bytes_median",
            Stat::DocumentBytesMean => "document_bytes_mean",
            Stat::DocumentBytesP90 => "document_bytes_p90",
            Stat::DocumentBytesMax => "document_bytes_max",
            Stat::HeadingLinesPerMebibyte => "heading_lines_per_mebibyte",
            Stat::LinksPerThousandDocuments => "links_per_thousand_documents",
            Stat::LinklessDocumentsPerMille => "linkless_documents_per_mille",
            Stat::NonMarkdownFilesPerMille => "non_markdown_files_per_mille",
            Stat::DocumentsPerThousandDirectories => "documents_per_thousand_directories",
            Stat::MaxDirectoryDepth => "max_directory_depth",
            Stat::AmbiguousStemClasses => "ambiguous_stem_classes",
            Stat::LargestStemClass => "largest_stem_class",
        }
    }

    pub fn read(self, stats: &VaultStats) -> u64 {
        match self {
            Stat::DocumentBytesP10 => stats.document_bytes_p10,
            Stat::DocumentBytesMedian => stats.document_bytes_median,
            Stat::DocumentBytesMean => stats.document_bytes_mean,
            Stat::DocumentBytesP90 => stats.document_bytes_p90,
            Stat::DocumentBytesMax => stats.document_bytes_max,
            Stat::HeadingLinesPerMebibyte => stats.heading_lines_per_mebibyte(),
            Stat::LinksPerThousandDocuments => stats.links_per_thousand_documents(),
            Stat::LinklessDocumentsPerMille => stats.linkless_documents_per_mille(),
            Stat::NonMarkdownFilesPerMille => stats.non_markdown_files_per_mille(),
            Stat::DocumentsPerThousandDirectories => stats.documents_per_thousand_directories(),
            Stat::MaxDirectoryDepth => stats.max_directory_depth,
            Stat::AmbiguousStemClasses => stats.ambiguous_stem_classes,
            Stat::LargestStemClass => stats.largest_stem_class,
        }
    }

    /// Every statistic the probe reports, so a listing cannot fall behind the
    /// enum.
    pub fn all() -> &'static [Stat] {
        &[
            Stat::DocumentBytesP10,
            Stat::DocumentBytesMedian,
            Stat::DocumentBytesMean,
            Stat::DocumentBytesP90,
            Stat::DocumentBytesMax,
            Stat::HeadingLinesPerMebibyte,
            Stat::LinksPerThousandDocuments,
            Stat::LinklessDocumentsPerMille,
            Stat::NonMarkdownFilesPerMille,
            Stat::DocumentsPerThousandDirectories,
            Stat::MaxDirectoryDepth,
            Stat::AmbiguousStemClasses,
            Stat::LargestStemClass,
        ]
    }
}

/// One entry of the calibration envelope: a statistic, the inclusive range it
/// is expected to land in, and the reason that range is where it is.
pub struct Target {
    pub stat: Stat,
    pub min: u64,
    pub max: u64,
    pub why: &'static str,
}

/// The realism envelope for a realistic-scale profile.
///
/// It constrains *shape*, never scale: document and directory counts are a
/// profile's own declaration and appear here only as ratios. A profile built
/// on a compressed length mixture — the cheap profiles tests use most — is
/// deliberately outside this envelope and is not checked against it.
pub const CALIBRATION: &[Target] = &[
    Target {
        stat: Stat::DocumentBytesP10,
        min: 150,
        max: 1_100,
        why: "the short end is a stub or a link hub, not an empty file",
    },
    Target {
        stat: Stat::DocumentBytesMedian,
        min: 700,
        max: 2_600,
        why: "the typical document is a page or two of prose",
    },
    Target {
        stat: Stat::DocumentBytesMean,
        min: 3_500,
        max: 6_500,
        why: "the mean sits well above the median because the tail is heavy; \
               a mean near the median is the signature of a bounded-uniform \
               generator, and it understates every per-byte cost",
    },
    Target {
        stat: Stat::DocumentBytesP90,
        min: 5_500,
        max: 22_000,
        why: "a tenth of documents are long-form: notes that accreted for months",
    },
    Target {
        stat: Stat::DocumentBytesMax,
        min: 15_000,
        max: 400_000,
        why: "the largest document is what peak memory is a function of, so a \
               tree without one cannot measure the memory invariant at all",
    },
    Target {
        stat: Stat::HeadingLinesPerMebibyte,
        min: 900,
        max: 4_200,
        why: "documents are sectioned rather than one wall of text, and not so \
               finely that every paragraph carries a heading",
    },
    Target {
        stat: Stat::LinksPerThousandDocuments,
        min: 3_000,
        max: 12_000,
        why: "three to twelve outbound links a document: a connected graph, not \
               a star and not a clique",
    },
    Target {
        stat: Stat::LinklessDocumentsPerMille,
        min: 0,
        max: 250,
        why: "isolated documents exist and are a minority; a tree with none \
               never exercises the no-neighbours path",
    },
    Target {
        stat: Stat::NonMarkdownFilesPerMille,
        min: 80,
        max: 350,
        why: "attachments, exports and editor state are a real share of the \
               files a walk pays for and the graph gains nothing from",
    },
    Target {
        stat: Stat::DocumentsPerThousandDirectories,
        min: 4_000,
        max: 40_000,
        why: "four to forty documents a directory: enough directories that walk \
               cost is visible, not so many that every one holds a single file",
    },
    Target {
        stat: Stat::MaxDirectoryDepth,
        min: 2,
        max: 6,
        why: "collections nest, and they stop nesting well short of a filesystem limit",
    },
    Target {
        stat: Stat::AmbiguousStemClasses,
        min: 5,
        max: 500,
        why: "repeated file names are ordinary; resolution has to face them",
    },
    Target {
        stat: Stat::LargestStemClass,
        min: 2,
        max: 64,
        why: "at least one name repeats often enough that resolving it bare is a \
               genuine choice among candidates",
    },
];

/// A statistic that landed outside its target range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deviation {
    pub stat: Stat,
    pub observed: u64,
    pub min: u64,
    pub max: u64,
}

impl std::fmt::Display for Deviation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} outside {}..={}",
            self.stat.name(),
            self.observed,
            self.min,
            self.max
        )
    }
}

/// Every target `stats` misses.
pub fn check(stats: &VaultStats, targets: &[Target]) -> Vec<Deviation> {
    targets
        .iter()
        .filter_map(|target| {
            let observed = target.stat.read(stats);
            (observed < target.min || observed > target.max).then_some(Deviation {
                stat: target.stat,
                observed,
                min: target.min,
                max: target.max,
            })
        })
        .collect()
}

/// Lower-nearest-rank percentile of a sorted, non-empty slice.
fn percentile(sorted: &[u64], p: u64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let index = (p * (sorted.len() as u64 - 1) / 100) as usize;
    sorted[index]
}

/// Count heading lines and `[[` occurrences in one document's bytes.
fn scan(bytes: &[u8]) -> (u64, u64) {
    let mut headings = 0;
    let mut links = 0;
    for line in bytes.split(|b| *b == b'\n') {
        let hashes = line.iter().take_while(|b| **b == b'#').count();
        if (1..=6).contains(&hashes) && line.get(hashes) == Some(&b' ') {
            headings += 1;
        }
        links += line.windows(2).filter(|w| w == b"[[").count() as u64;
    }
    (headings, links)
}

/// Measure the shape of the tree rooted at `root`.
pub fn measure(root: &Path) -> io::Result<VaultStats> {
    let mut stats = VaultStats::default();
    let mut sizes: Vec<u64> = Vec::new();
    let mut stems: BTreeMap<String, u64> = BTreeMap::new();

    for node in tree::walk(root)? {
        if node.is_dir {
            stats.directories += 1;
            let depth = node.rel.split('/').count() as u64;
            stats.max_directory_depth = stats.max_directory_depth.max(depth);
            continue;
        }
        let Some(stem) = node.rel.strip_suffix(".md") else {
            stats.non_markdown_files += 1;
            continue;
        };
        let stem = stem.rsplit('/').next().unwrap_or(stem).to_string();
        *stems.entry(stem).or_insert(0) += 1;

        let bytes = fs::read(&node.path)?;
        let (headings, links) = scan(&bytes);
        stats.documents += 1;
        stats.markdown_bytes += bytes.len() as u64;
        stats.heading_lines += headings;
        stats.links += links;
        if links == 0 {
            stats.linkless_documents += 1;
        }
        sizes.push(bytes.len() as u64);
    }

    sizes.sort_unstable();
    stats.document_bytes_p10 = percentile(&sizes, 10);
    stats.document_bytes_median = percentile(&sizes, 50);
    stats.document_bytes_p90 = percentile(&sizes, 90);
    stats.document_bytes_max = sizes.last().copied().unwrap_or(0);
    stats.document_bytes_mean = ratio(stats.markdown_bytes, stats.documents);

    for count in stems.values() {
        if *count > 1 {
            stats.ambiguous_stem_classes += 1;
            stats.largest_stem_class = stats.largest_stem_class.max(*count);
        }
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_target_names_a_nonempty_range_and_a_reason() {
        for target in CALIBRATION {
            assert!(
                target.min <= target.max,
                "{} runs backwards",
                target.stat.name()
            );
            assert!(
                !target.why.is_empty(),
                "{} states no reason",
                target.stat.name()
            );
        }
    }

    #[test]
    fn no_statistic_is_constrained_twice() {
        let mut seen: Vec<&str> = CALIBRATION.iter().map(|t| t.stat.name()).collect();
        seen.sort_unstable();
        let total = seen.len();
        seen.dedup();
        assert_eq!(total, seen.len(), "a statistic is constrained twice");
    }

    #[test]
    fn statistic_names_are_unique() {
        let mut names: Vec<&str> = Stat::all().iter().map(|s| s.name()).collect();
        names.sort_unstable();
        let total = names.len();
        names.dedup();
        assert_eq!(total, names.len(), "two statistics share a name");
    }

    #[test]
    fn headings_are_counted_by_their_leading_bytes() {
        let (headings, _) = scan(b"# One\n## Two\n###### Six\n####### Seven\n#NoSpace\ntext\n");
        assert_eq!(headings, 3);
    }

    #[test]
    fn links_are_counted_by_occurrence() {
        let (_, links) = scan(b"see [[a]] and [[b|c]]\nno link here\n[[d]]\n");
        assert_eq!(links, 3);
    }

    #[test]
    fn percentiles_bracket_a_known_series() {
        let series: Vec<u64> = (1..=101).collect();
        assert_eq!(percentile(&series, 0), 1);
        assert_eq!(percentile(&series, 50), 51);
        assert_eq!(percentile(&series, 100), 101);
    }

    #[test]
    fn check_reports_only_what_falls_outside() {
        let stats = VaultStats {
            document_bytes_mean: 10,
            ..VaultStats::default()
        };
        let targets = [
            Target {
                stat: Stat::DocumentBytesMean,
                min: 1,
                max: 5,
                why: "deliberately missed",
            },
            Target {
                stat: Stat::DocumentBytesMax,
                min: 0,
                max: 5,
                why: "deliberately met",
            },
        ];
        let deviations = check(&stats, &targets);
        assert_eq!(deviations.len(), 1);
        assert_eq!(deviations[0].stat, Stat::DocumentBytesMean);
        assert_eq!(deviations[0].observed, 10);
    }
}
