//! The calibration probe: what a tree's realism statistics are, and the
//! envelope they are expected to land in.
//!
//! # The probe is a shape meter, not a parser
//!
//! [`measure`] reads bytes and counts patterns. It does not interpret them:
//! it never resolves a link, never decides what a frontmatter field means,
//! never honours a code fence or an escape. A heading is a line whose first
//! bytes are one to six `#` followed by a space; a link is an occurrence of
//! `[[`, and its spelling is whether the bytes up to the next `]]` carry a
//! `/` or a `|`. Document syntax has an owner elsewhere in the workspace, and
//! a second interpretation of it here would be a second grammar. What the probe
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
    /// Documents carrying a level-one heading.
    pub title_heading_documents: u64,
    pub links: u64,
    /// Links whose target name carries a `/` — the path-qualified spelling.
    pub path_qualified_links: u64,
    /// Links whose target carries display text after a `|`.
    pub aliased_links: u64,
    pub linkless_documents: u64,
    /// Documents whose file name is not pure ASCII.
    pub non_ascii_named_documents: u64,
    /// Documents whose file name carries an interior space.
    pub spaced_name_documents: u64,
    /// Documents sitting at the tree root rather than inside a directory.
    pub root_documents: u64,
    /// Non-Markdown files whose bytes are not valid UTF-8.
    pub binary_files: u64,
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

    /// Path-qualified links, per mille of links.
    pub fn path_qualified_links_per_mille(&self) -> u64 {
        ratio(self.path_qualified_links * 1000, self.links)
    }

    /// Links carrying display text, per mille of links.
    pub fn aliased_links_per_mille(&self) -> u64 {
        ratio(self.aliased_links * 1000, self.links)
    }

    /// Documents carrying no links at all, per mille of documents.
    pub fn linkless_documents_per_mille(&self) -> u64 {
        ratio(self.linkless_documents * 1000, self.documents)
    }

    /// Documents whose name is not pure ASCII, per mille of documents.
    pub fn non_ascii_named_documents_per_mille(&self) -> u64 {
        ratio(self.non_ascii_named_documents * 1000, self.documents)
    }

    /// Documents whose name carries a space, per mille of documents.
    pub fn spaced_name_documents_per_mille(&self) -> u64 {
        ratio(self.spaced_name_documents * 1000, self.documents)
    }

    /// Documents at the tree root, per mille of documents.
    pub fn root_documents_per_mille(&self) -> u64 {
        ratio(self.root_documents * 1000, self.documents)
    }

    /// Documents opening a level-one heading, per mille of documents.
    pub fn title_heading_documents_per_mille(&self) -> u64 {
        ratio(self.title_heading_documents * 1000, self.documents)
    }

    /// Non-Markdown files that are not text, per mille of non-Markdown files.
    pub fn binary_files_per_mille(&self) -> u64 {
        ratio(self.binary_files * 1000, self.non_markdown_files)
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
    TitleHeadingDocumentsPerMille,
    LinksPerThousandDocuments,
    PathQualifiedLinksPerMille,
    AliasedLinksPerMille,
    LinklessDocumentsPerMille,
    NonAsciiNamedDocumentsPerMille,
    SpacedNameDocumentsPerMille,
    RootDocumentsPerMille,
    NonMarkdownFilesPerMille,
    BinaryFilesPerMille,
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
            Stat::TitleHeadingDocumentsPerMille => "title_heading_documents_per_mille",
            Stat::LinksPerThousandDocuments => "links_per_thousand_documents",
            Stat::PathQualifiedLinksPerMille => "path_qualified_links_per_mille",
            Stat::AliasedLinksPerMille => "aliased_links_per_mille",
            Stat::LinklessDocumentsPerMille => "linkless_documents_per_mille",
            Stat::NonAsciiNamedDocumentsPerMille => "non_ascii_named_documents_per_mille",
            Stat::SpacedNameDocumentsPerMille => "spaced_name_documents_per_mille",
            Stat::RootDocumentsPerMille => "root_documents_per_mille",
            Stat::NonMarkdownFilesPerMille => "non_markdown_files_per_mille",
            Stat::BinaryFilesPerMille => "binary_files_per_mille",
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
            Stat::TitleHeadingDocumentsPerMille => stats.title_heading_documents_per_mille(),
            Stat::LinksPerThousandDocuments => stats.links_per_thousand_documents(),
            Stat::PathQualifiedLinksPerMille => stats.path_qualified_links_per_mille(),
            Stat::AliasedLinksPerMille => stats.aliased_links_per_mille(),
            Stat::LinklessDocumentsPerMille => stats.linkless_documents_per_mille(),
            Stat::NonAsciiNamedDocumentsPerMille => stats.non_ascii_named_documents_per_mille(),
            Stat::SpacedNameDocumentsPerMille => stats.spaced_name_documents_per_mille(),
            Stat::RootDocumentsPerMille => stats.root_documents_per_mille(),
            Stat::NonMarkdownFilesPerMille => stats.non_markdown_files_per_mille(),
            Stat::BinaryFilesPerMille => stats.binary_files_per_mille(),
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
            Stat::TitleHeadingDocumentsPerMille,
            Stat::LinksPerThousandDocuments,
            Stat::PathQualifiedLinksPerMille,
            Stat::AliasedLinksPerMille,
            Stat::LinklessDocumentsPerMille,
            Stat::NonAsciiNamedDocumentsPerMille,
            Stat::SpacedNameDocumentsPerMille,
            Stat::RootDocumentsPerMille,
            Stat::NonMarkdownFilesPerMille,
            Stat::BinaryFilesPerMille,
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
        stat: Stat::TitleHeadingDocumentsPerMille,
        min: 100,
        max: 980,
        why: "most documents repeat their title as a level-one heading and some \
               do not, so a reader of raw bytes meets both the titled and the \
               untitled shape",
    },
    Target {
        stat: Stat::LinksPerThousandDocuments,
        min: 3_000,
        max: 12_000,
        why: "three to twelve outbound links a document: a connected graph, not \
               a star and not a clique",
    },
    Target {
        stat: Stat::PathQualifiedLinksPerMille,
        min: 100,
        max: 500,
        why: "both link spellings are in real use: a bare stem leans on \
               resolution, a path-qualified target bypasses it, and a tree \
               carrying only one never exercises the other",
    },
    Target {
        stat: Stat::AliasedLinksPerMille,
        min: 100,
        max: 550,
        why: "display text is ordinary in prose, and it is the case where the \
               text a reader sees and the target a tool resolves differ",
    },
    Target {
        stat: Stat::LinklessDocumentsPerMille,
        min: 20,
        max: 250,
        why: "isolated documents exist and are a minority. The floor is the \
               load-bearing half: a tree where every document links somewhere \
               never exercises the no-neighbours path at all",
    },
    Target {
        stat: Stat::NonAsciiNamedDocumentsPerMille,
        min: 15,
        max: 200,
        why: "collections are written in more than one language, and a \
               non-ASCII file name is where path handling, normalization and \
               stem indexing are actually tested",
    },
    Target {
        stat: Stat::SpacedNameDocumentsPerMille,
        min: 15,
        max: 200,
        why: "file names with spaces are normal in a human-curated collection \
               and are the ones that break naive quoting and tokenizing",
    },
    Target {
        stat: Stat::RootDocumentsPerMille,
        min: 10,
        max: 250,
        why: "some documents never get filed. A tree whose every document sits \
               in a directory misses the shallowest path case",
    },
    Target {
        stat: Stat::NonMarkdownFilesPerMille,
        min: 80,
        max: 350,
        why: "attachments, exports and editor state are a real share of the \
               files a walk pays for and the graph gains nothing from",
    },
    Target {
        stat: Stat::BinaryFilesPerMille,
        min: 150,
        max: 800,
        why: "attachments are a mix of text exports and real binaries; a tree \
               whose every non-Markdown file decodes as text never exercises \
               the branch a walk takes when one does not",
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
        min: 6,
        max: 64,
        why: "a finding carries a bounded head of five candidates, so a class of \
               five or fewer fits inside the bound and never shows it \
               truncating. The floor sits above the bound deliberately: a \
               profile measured by the gates has to meet the truncating case, \
               not merely approach it",
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

/// What one document's bytes carry: heading lines, and each link's spelling.
#[derive(Default)]
struct Scan {
    headings: u64,
    title_heading: bool,
    links: u64,
    path_qualified_links: u64,
    aliased_links: u64,
}

/// Count heading lines and wikilink spellings in one document's bytes.
///
/// A link runs from `[[` to the next `]]` on the same line. Inside it, a `/`
/// means the target was written path-qualified and a `|` means it carries
/// display text — both byte tests over the span, never an interpretation of
/// what the target resolves to.
fn scan(bytes: &[u8]) -> Scan {
    let mut out = Scan::default();
    for line in bytes.split(|b| *b == b'\n') {
        let hashes = line.iter().take_while(|b| **b == b'#').count();
        if (1..=6).contains(&hashes) && line.get(hashes) == Some(&b' ') {
            out.headings += 1;
            if hashes == 1 {
                out.title_heading = true;
            }
        }
        let mut cursor = 0;
        while cursor + 1 < line.len() {
            if &line[cursor..cursor + 2] != b"[[" {
                cursor += 1;
                continue;
            }
            let open = cursor + 2;
            let close = (open..line.len().saturating_sub(1))
                .find(|i| &line[*i..*i + 2] == b"]]")
                .unwrap_or(line.len());
            let span = &line[open..close.min(line.len())];
            out.links += 1;
            if span.contains(&b'/') {
                out.path_qualified_links += 1;
            }
            if span.contains(&b'|') {
                out.aliased_links += 1;
            }
            // Advance past the opener, not past the span: the link count stays
            // a count of `[[` occurrences, so an unclosed opener cannot
            // swallow the links that follow it.
            cursor = open;
        }
    }
    out
}

/// Measure the shape of the tree rooted at `root`.
#[allow(clippy::disallowed_methods)] // The probe reads the bytes it measures.
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
            // Text or not is a byte test — whether the bytes decode — never a
            // question about what the file means.
            if std::str::from_utf8(&fs::read(&node.path)?).is_err() {
                stats.binary_files += 1;
            }
            continue;
        };
        let leaf = stem.rsplit('/').next().unwrap_or(stem).to_string();
        if !leaf.is_ascii() {
            stats.non_ascii_named_documents += 1;
        }
        if leaf.contains(' ') {
            stats.spaced_name_documents += 1;
        }
        if !node.rel.contains('/') {
            stats.root_documents += 1;
        }
        *stems.entry(leaf).or_insert(0) += 1;

        let bytes = fs::read(&node.path)?;
        let scanned = scan(&bytes);
        stats.documents += 1;
        stats.markdown_bytes += bytes.len() as u64;
        stats.heading_lines += scanned.headings;
        if scanned.title_heading {
            stats.title_heading_documents += 1;
        }
        stats.links += scanned.links;
        stats.path_qualified_links += scanned.path_qualified_links;
        stats.aliased_links += scanned.aliased_links;
        if scanned.links == 0 {
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

    /// Every statistic the probe reports carries an envelope entry.
    ///
    /// Without this, adding a statistic and forgetting its target leaves a
    /// number that is printed and never judged — which is how a knob comes to
    /// manifest in trees while nothing fails when it stops.
    #[test]
    fn every_statistic_carries_a_target() {
        let constrained: Vec<&str> = CALIBRATION.iter().map(|t| t.stat.name()).collect();
        let unconstrained: Vec<&str> = Stat::all()
            .iter()
            .map(|s| s.name())
            .filter(|name| !constrained.contains(name))
            .collect();
        assert!(
            unconstrained.is_empty(),
            "these statistics are reported and never judged: {unconstrained:?}"
        );
    }

    /// And the other direction: a target naming a statistic the probe does not
    /// report would be a range nothing is ever measured against.
    #[test]
    fn every_target_names_a_reported_statistic() {
        let reported: Vec<&str> = Stat::all().iter().map(|s| s.name()).collect();
        for target in CALIBRATION {
            assert!(
                reported.contains(&target.stat.name()),
                "{} is constrained but not reported",
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
        let scanned = scan(b"# One\n## Two\n###### Six\n####### Seven\n#NoSpace\ntext\n");
        assert_eq!(scanned.headings, 3);
    }

    #[test]
    fn links_are_counted_by_occurrence() {
        let scanned = scan(b"see [[a]] and [[b|c]]\nno link here\n[[d]]\n");
        assert_eq!(scanned.links, 3);
    }

    #[test]
    fn link_spellings_are_counted_inside_their_own_span() {
        let scanned =
            scan(b"[[bare]] [[dir/sub/qualified]] [[aliased|shown]] [[dir/both|shown]]\n");
        assert_eq!(scanned.links, 4);
        assert_eq!(scanned.path_qualified_links, 2);
        assert_eq!(scanned.aliased_links, 2);
    }

    /// A slash or a bar outside a link belongs to the prose, not to a target.
    #[test]
    fn text_around_a_link_does_not_count_as_a_spelling() {
        let scanned = scan(b"a/b | c [[bare]] d/e | f\n");
        assert_eq!(scanned.links, 1);
        assert_eq!(scanned.path_qualified_links, 0);
        assert_eq!(scanned.aliased_links, 0);
    }

    #[test]
    fn an_unclosed_link_does_not_swallow_the_next_one() {
        let scanned = scan(b"[[open and [[closed]]\n");
        assert_eq!(scanned.links, 2);
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
