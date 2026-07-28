//! The realism knobs and the named profiles built from them.
//!
//! A [`Profile`] is data: a name, a document count, and one setting per knob.
//! Profiles are `const` because a generated tree is a pure function of
//! `(profile, seed)` — a profile that could be assembled at run time from an
//! environment variable or a config file would put a value outside the pair
//! into the tree, and the determinism contract would stop meaning anything.
//!
//! Every field is either a count or a per-mille rate, and every rate is drawn
//! against the seeded generator. Integer arithmetic throughout: no floating
//! point enters a generated tree, so no rounding difference can move a byte
//! between platforms.

/// One bucket of the body-length distribution: a weight, and the byte range a
/// draw landing in this bucket picks uniformly from.
#[derive(Clone, Copy)]
pub struct LengthBucket {
    /// Share of documents landing in this bucket, per mille.
    pub weight_per_mille: u32,
    pub min_bytes: usize,
    pub max_bytes: usize,
}

/// **Knob 1 — body length.** How long a generated document's body is, and how
/// it is punctuated.
///
/// The distribution is a weighted mixture of byte ranges rather than a bounded
/// uniform, because document length in a real collection is long-tailed: most
/// documents are short, a meaningful minority are long, and a handful are very
/// long indeed. A generator whose bodies are a bounded uniform of a few short
/// paragraphs produces a mean around a couple of hundred bytes — roughly a
/// twentieth of a realistic document — and every cost measured against such a
/// tree understates the real cost by about that factor: fewer bytes to read,
/// fewer to parse, fewer to store, and a tail that never appears at all. The
/// tail is the part that matters most, because peak-memory and streaming
/// behaviour are properties of the largest document, not the average one.
#[derive(Clone, Copy)]
pub struct BodyShape {
    /// The mixture. Weights sum to 1000.
    pub length: &'static [LengthBucket],
    /// One heading is emitted per this many body bytes.
    pub bytes_per_heading: usize,
    /// Per-mille of documents opening with a level-one heading below the
    /// frontmatter.
    pub title_heading_per_mille: u32,
}

/// **Knob 2 — ambiguity classes.** How many distinct stems repeat across
/// directories, and how many documents share each one.
///
/// `k` is the size of a class: `k` documents in `k` different directories with
/// the same file name. It is the knob that makes bare-stem resolution
/// genuinely ambiguous, and the one a bounded candidate list is judged
/// against — a class larger than the bound is what proves the bound truncates
/// rather than merely fits.
#[derive(Clone, Copy)]
pub struct Ambiguity {
    /// Number of repeating stems. Clamped to the size of the stem pool.
    pub classes: usize,
    /// Documents per class. Clamped to the number of available directories,
    /// since a class places each of its members in a distinct one.
    pub k: usize,
}

/// **Knob 3 — link density.** How many wikilinks a document carries, in what
/// forms, and how many of them resolve to nothing.
#[derive(Clone, Copy)]
pub struct LinkDensity {
    /// Mean outbound links per document. Draws are uniform over
    /// `0..=2 * mean_per_doc`, capped at `max_per_doc`.
    pub mean_per_doc: usize,
    pub max_per_doc: usize,
    /// Per-mille of emitted links naming a target that does not exist.
    pub dangling_per_mille: u32,
    /// Per-mille of emitted links written path-qualified rather than as a
    /// bare stem.
    pub path_qualified_per_mille: u32,
    /// Per-mille of emitted links carrying display text (`[[target|text]]`).
    pub aliased_per_mille: u32,
    /// Per-mille of a document's links gathered into a trailing list rather
    /// than sitting inline in a paragraph.
    pub trailing_list_per_mille: u32,
}

/// **Knob 4 — directory shape.** The folder tree documents are placed into,
/// how they distribute across it, and what their leaf names look like.
///
/// Walk cost scales with directories, not only with documents, so the count
/// and depth of directories is a first-class dial. Placement is skewed rather
/// than uniform because a real vault has a few crowded folders and a long tail
/// of sparse ones, and a uniform spread hides every cost that depends on a
/// directory being large.
#[derive(Clone, Copy)]
pub struct DirShape {
    /// Top-level directories. Clamped to the size of the name pool.
    pub top_level: usize,
    /// Nesting levels below the top-level directories.
    pub depth: usize,
    /// Sub-directories created per directory at each level.
    pub fan_out: usize,
    /// Per-mille of documents placed at the vault root.
    pub root_per_mille: u32,
    /// Placement concentration. `n` takes the lowest of `n + 1` draws, so a
    /// higher value crowds the earlier directories; `0` is uniform.
    pub placement_skew: u32,
    /// Per-mille of document stems carrying non-ASCII characters.
    pub unicode_stem_per_mille: u32,
    /// Per-mille of document stems carrying interior spaces.
    pub spaced_stem_per_mille: u32,
}

/// **Knob 5 — non-Markdown clutter.** Everything in a vault that is not a
/// document: attachments, exports, editor state, and directories holding none
/// of the above.
///
/// A walk pays for these and a graph gains nothing from them, so a tree
/// without them measures a walk that does not exist.
#[derive(Clone, Copy)]
pub struct Clutter {
    /// Non-Markdown files emitted per thousand documents.
    pub files_per_thousand_docs: u32,
    /// Per-mille of those files whose bytes are not valid UTF-8.
    pub binary_per_mille: u32,
    /// Largest clutter file, in bytes. Sizes are drawn uniformly below this.
    pub max_file_bytes: usize,
    /// Directories created holding no documents at all.
    pub empty_dirs: usize,
}

/// A named generation profile: a document count plus one setting per knob.
#[derive(Clone, Copy)]
pub struct Profile {
    pub name: &'static str,
    /// One-line statement of what this profile is for.
    pub purpose: &'static str,
    /// Markdown documents in the generated tree, ambiguity classes included.
    pub docs: usize,
    pub body: BodyShape,
    pub ambiguity: Ambiguity,
    pub links: LinkDensity,
    pub dirs: DirShape,
    pub clutter: Clutter,
}

/// The long-tailed body-length mixture. Weights sum to 1000; the weighted mean
/// of the bucket midpoints is about 4.5 KiB, with a tail reaching 70 KiB.
pub const LONG_TAILED_BODY: &[LengthBucket] = &[
    LengthBucket {
        weight_per_mille: 195,
        min_bytes: 120,
        max_bytes: 500,
    },
    LengthBucket {
        weight_per_mille: 350,
        min_bytes: 500,
        max_bytes: 1_800,
    },
    LengthBucket {
        weight_per_mille: 300,
        min_bytes: 1_800,
        max_bytes: 6_500,
    },
    LengthBucket {
        weight_per_mille: 130,
        min_bytes: 6_500,
        max_bytes: 20_000,
    },
    LengthBucket {
        weight_per_mille: 25,
        min_bytes: 20_000,
        max_bytes: 70_000,
    },
];

/// A compressed mixture for the profiles whose job is to be small and fast.
/// The same long-tailed shape at a fraction of the scale, so a cheap profile
/// still exercises short, medium and long documents.
pub const COMPACT_BODY: &[LengthBucket] = &[
    LengthBucket {
        weight_per_mille: 600,
        min_bytes: 120,
        max_bytes: 600,
    },
    LengthBucket {
        weight_per_mille: 330,
        min_bytes: 600,
        max_bytes: 2_500,
    },
    LengthBucket {
        weight_per_mille: 70,
        min_bytes: 2_500,
        max_bytes: 9_000,
    },
];

const REALISTIC_LINKS: LinkDensity = LinkDensity {
    mean_per_doc: 6,
    max_per_doc: 24,
    dangling_per_mille: 60,
    path_qualified_per_mille: 250,
    aliased_per_mille: 300,
    trailing_list_per_mille: 350,
};

const REALISTIC_CLUTTER: Clutter = Clutter {
    files_per_thousand_docs: 180,
    binary_per_mille: 400,
    max_file_bytes: 24_000,
    empty_dirs: 8,
};

const REALISTIC_BODY: BodyShape = BodyShape {
    length: LONG_TAILED_BODY,
    bytes_per_heading: 700,
    title_heading_per_mille: 700,
};

/// The named profiles, in a fixed order. This table is the whole set: a
/// profile is added here, reviewed as a diff, and named by tests thereafter.
pub const PROFILES: &[Profile] = &[
    // Small enough to read in full when a test fails.
    Profile {
        name: "tiny",
        purpose: "unit-scale scaffolding; small enough to inspect by hand",
        docs: 12,
        body: BodyShape {
            length: COMPACT_BODY,
            bytes_per_heading: 500,
            title_heading_per_mille: 700,
        },
        ambiguity: Ambiguity { classes: 2, k: 2 },
        links: LinkDensity {
            mean_per_doc: 3,
            max_per_doc: 8,
            dangling_per_mille: 100,
            path_qualified_per_mille: 250,
            aliased_per_mille: 250,
            trailing_list_per_mille: 500,
        },
        dirs: DirShape {
            top_level: 3,
            depth: 1,
            fan_out: 2,
            root_per_mille: 80,
            placement_skew: 1,
            unicode_stem_per_mille: 80,
            spaced_stem_per_mille: 80,
        },
        clutter: Clutter {
            files_per_thousand_docs: 250,
            binary_per_mille: 400,
            max_file_bytes: 2_000,
            empty_dirs: 2,
        },
    },
    // The everyday integration-test profile: realistic in shape, cheap enough
    // to generate several times in one test run.
    Profile {
        name: "small",
        purpose: "integration-scale; realistic shape at a cost tests can pay repeatedly",
        docs: 120,
        body: BodyShape {
            length: COMPACT_BODY,
            bytes_per_heading: 600,
            title_heading_per_mille: 700,
        },
        ambiguity: Ambiguity { classes: 6, k: 3 },
        links: REALISTIC_LINKS,
        dirs: DirShape {
            top_level: 6,
            depth: 2,
            fan_out: 2,
            root_per_mille: 50,
            placement_skew: 2,
            unicode_stem_per_mille: 60,
            spaced_stem_per_mille: 60,
        },
        clutter: Clutter {
            files_per_thousand_docs: 180,
            binary_per_mille: 400,
            max_file_bytes: 8_000,
            empty_dirs: 4,
        },
    },
    // Ambiguity past the point where a bounded candidate list has to truncate.
    Profile {
        name: "ambiguous",
        purpose: "stem resolution under large ambiguity classes",
        docs: 300,
        body: BodyShape {
            length: COMPACT_BODY,
            bytes_per_heading: 600,
            title_heading_per_mille: 700,
        },
        ambiguity: Ambiguity { classes: 12, k: 12 },
        links: LinkDensity {
            mean_per_doc: 8,
            max_per_doc: 24,
            dangling_per_mille: 60,
            // Half the links are path-qualified: an ambiguity profile has to
            // exercise the disambiguating form as well as the ambiguous one.
            path_qualified_per_mille: 500,
            aliased_per_mille: 200,
            trailing_list_per_mille: 350,
        },
        dirs: DirShape {
            top_level: 6,
            depth: 2,
            fan_out: 3,
            root_per_mille: 40,
            placement_skew: 1,
            unicode_stem_per_mille: 60,
            spaced_stem_per_mille: 60,
        },
        clutter: Clutter {
            files_per_thousand_docs: 120,
            binary_per_mille: 400,
            max_file_bytes: 8_000,
            empty_dirs: 4,
        },
    },
    // The profile per-PR measurement gates run against.
    Profile {
        name: "realistic",
        purpose: "the ~2k-document measurement profile the per-PR gates assert against",
        docs: 2_000,
        body: REALISTIC_BODY,
        ambiguity: Ambiguity { classes: 20, k: 3 },
        links: REALISTIC_LINKS,
        dirs: DirShape {
            top_level: 10,
            depth: 2,
            fan_out: 3,
            root_per_mille: 40,
            placement_skew: 2,
            unicode_stem_per_mille: 50,
            spaced_stem_per_mille: 50,
        },
        clutter: REALISTIC_CLUTTER,
    },
    // The soak profile: more documents, more directories, larger classes.
    Profile {
        name: "soak",
        purpose: "the >=5k-document profile the scheduled soak lane runs",
        docs: 6_000,
        body: REALISTIC_BODY,
        ambiguity: Ambiguity { classes: 24, k: 5 },
        links: LinkDensity {
            mean_per_doc: 8,
            max_per_doc: 40,
            dangling_per_mille: 60,
            path_qualified_per_mille: 250,
            aliased_per_mille: 300,
            trailing_list_per_mille: 350,
        },
        dirs: DirShape {
            top_level: 10,
            depth: 3,
            fan_out: 4,
            root_per_mille: 30,
            placement_skew: 3,
            unicode_stem_per_mille: 50,
            spaced_stem_per_mille: 50,
        },
        clutter: Clutter {
            files_per_thousand_docs: 220,
            binary_per_mille: 400,
            max_file_bytes: 64_000,
            empty_dirs: 12,
        },
    },
];

impl Profile {
    /// Look up a named profile.
    pub fn by_name(name: &str) -> Option<Profile> {
        PROFILES.iter().copied().find(|p| p.name == name)
    }

    /// Every named profile, in the table's order.
    pub fn all() -> &'static [Profile] {
        PROFILES
    }

    /// Every profile name, in the table's order.
    pub fn names() -> Vec<&'static str> {
        PROFILES.iter().map(|p| p.name).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::words::AMBIGUOUS_STEMS;

    #[test]
    fn every_length_mixture_sums_to_one_thousand() {
        for mixture in [LONG_TAILED_BODY, COMPACT_BODY] {
            let total: u32 = mixture.iter().map(|b| b.weight_per_mille).sum();
            assert_eq!(total, 1000, "a length mixture must sum to 1000 per mille");
        }
    }

    #[test]
    fn every_length_bucket_is_a_nonempty_range() {
        for mixture in [LONG_TAILED_BODY, COMPACT_BODY] {
            for bucket in mixture {
                assert!(
                    bucket.max_bytes >= bucket.min_bytes,
                    "a length bucket must not run backwards"
                );
            }
        }
    }

    /// The distribution's headline property, asserted as arithmetic rather
    /// than left to a comment: the long-tailed mixture averages about 4.5 KiB
    /// per body, roughly twenty times a few-short-paragraphs body.
    #[test]
    fn the_long_tailed_mixture_means_about_four_and_a_half_kibibytes() {
        let mean: u64 = LONG_TAILED_BODY
            .iter()
            .map(|b| {
                u64::from(b.weight_per_mille) * ((b.min_bytes + b.max_bytes) as u64 / 2) / 1000
            })
            .sum();
        assert!(
            (4_000..=5_200).contains(&mean),
            "long-tailed mixture mean drifted to {mean} bytes"
        );
    }

    #[test]
    fn profile_names_are_unique() {
        let mut names = Profile::names();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "two profiles share a name");
    }

    #[test]
    fn the_gate_and_soak_profiles_hold_their_declared_scale() {
        let realistic = Profile::by_name("realistic").expect("realistic profile");
        assert!(
            (1_500..=2_500).contains(&realistic.docs),
            "the per-PR gate profile is a ~2k-document profile"
        );
        let soak = Profile::by_name("soak").expect("soak profile");
        assert!(soak.docs >= 5_000, "the soak profile is a >=5k profile");
    }

    #[test]
    fn no_profile_asks_for_more_classes_than_the_stem_pool_holds() {
        for profile in Profile::all() {
            assert!(
                profile.ambiguity.classes <= AMBIGUOUS_STEMS.len(),
                "{} asks for more ambiguity classes than the stem pool holds",
                profile.name
            );
        }
    }

    #[test]
    fn ambiguity_classes_never_claim_more_documents_than_a_profile_has() {
        for profile in Profile::all() {
            let claimed = profile.ambiguity.classes * profile.ambiguity.k;
            assert!(
                claimed <= profile.docs,
                "{}'s ambiguity classes claim {claimed} of {} documents",
                profile.name,
                profile.docs
            );
        }
    }
}
