//! Knobs whose effect the calibration envelope does not reach.
//!
//! Most knobs are constrained by a probe statistic with an envelope entry
//! around it. Three are not, because what they change is not a shape statistic
//! a probe reading raw bytes can name without learning this generator's own
//! phrasing. They are turned directly here instead: each case generates twice
//! from one profile differing in one field, and requires the tree to respond.
//!
//! A knob nothing fails over is decoration, and the way that happens is not
//! malice — it is a field that quietly stops being read.

mod scratch;

use std::collections::BTreeMap;
use std::path::Path;

use norn_fixtures::Profile;

use scratch::with_tree;

fn profile(name: &str) -> Profile {
    Profile::by_name(name).unwrap_or_else(|| panic!("no profile named `{name}`"))
}

/// Occurrences of the inline link lead-in across the whole tree.
fn inline_link_lead_ins(dir: &Path) -> usize {
    scratch::document_texts(dir)
        .iter()
        .map(|text| text.matches("\nRelated: ").count())
        .sum()
}

/// The largest number of documents any single directory holds.
fn fullest_directory(dir: &Path) -> usize {
    let mut per_directory: BTreeMap<String, usize> = BTreeMap::new();
    for document in scratch::documents(dir) {
        *per_directory
            .entry(document.parent().to_string())
            .or_insert(0) += 1;
    }
    per_directory.values().copied().max().unwrap_or(0)
}

/// Where a document's links sit is what this knob decides, and a probe reading
/// raw bytes cannot name the difference without learning the generator's own
/// heading text. Turned directly: at zero every document places its links
/// inline, at a thousand none does.
#[test]
fn the_trailing_list_share_decides_where_links_sit() {
    let base = profile("small");

    let mut always = base;
    always.links.trailing_list_per_mille = 1000;
    let gathered = with_tree("knob-trailing-all", &always, 3, |dir, manifest| {
        assert!(manifest.links > 0, "the sample emitted no links");
        inline_link_lead_ins(dir)
    });

    let mut never = base;
    never.links.trailing_list_per_mille = 0;
    let inline = with_tree("knob-trailing-none", &never, 3, |dir, _| {
        inline_link_lead_ins(dir)
    });

    assert_eq!(
        gathered, 0,
        "every link should be gathered into a trailing list at 1000 per mille"
    );
    assert!(
        inline > 0,
        "no link was placed inline at 0 per mille, so the knob is not being read"
    );
}

/// Directories holding no documents are the walk cost a graph gains nothing
/// from. They are indistinguishable in a probe statistic from directories that
/// merely ended up empty, so the knob is turned directly: each requested entry
/// creates at least one directory that would not otherwise exist.
#[test]
fn document_free_directories_are_created_because_the_knob_asks() {
    let base = profile("small");
    let requested = base.clutter.empty_dirs;
    assert!(requested > 0, "the sample profile asks for none");

    let with = with_tree("knob-empty-dirs-on", &base, 5, |_, manifest| {
        manifest.directories
    });

    let mut without = base;
    without.clutter.empty_dirs = 0;
    let bare = with_tree("knob-empty-dirs-off", &without, 5, |_, manifest| {
        manifest.directories
    });

    assert!(
        with >= bare + requested,
        "asking for {requested} document-free directories added {} of them",
        with.saturating_sub(bare)
    );
}

/// Placement skew crowds the earlier directories. The envelope constrains how
/// many documents a directory holds on average; the crowding itself is a
/// property of the distribution's tail, which an average cannot see.
#[test]
fn placement_skew_crowds_the_fullest_directory() {
    let base = profile("small");

    let mut uniform = base;
    uniform.dirs.placement_skew = 0;
    let spread = with_tree("knob-skew-none", &uniform, 7, |dir, _| {
        fullest_directory(dir)
    });

    let mut crowded = base;
    crowded.dirs.placement_skew = 4;
    let concentrated = with_tree("knob-skew-high", &crowded, 7, |dir, _| {
        fullest_directory(dir)
    });

    assert!(
        concentrated > spread,
        "skew 4 filled its fullest directory with {concentrated} documents \
         against {spread} at skew 0, so the knob is not being read"
    );
}
