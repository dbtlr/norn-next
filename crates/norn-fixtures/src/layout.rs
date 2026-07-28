//! Where documents go: the directory tree, the ambiguity classes, and the
//! placement of every document into a path.
//!
//! Planning runs to completion before any body is written, which is what lets
//! a document link to a document generated after it. A forward-only generator
//! that can only link backwards leaves its earliest documents link-poor and
//! its latest link-rich, and a link-density knob set on such a generator does
//! not mean what it says.

use crate::profile::{DirShape, Profile};
use crate::rng::Rng;
use crate::words::{
    AMBIGUOUS_STEMS, DOC_WORDS, SPACED_STEMS, SUB_DIRS, TOP_LEVEL_DIRS, UNICODE_STEMS,
};

/// One planned document.
pub struct Placed {
    /// Vault-relative path, forward-slash, with the `.md` extension.
    pub path: String,
    /// File-name stem, the bare form a stem link names.
    pub stem: String,
    /// Vault-relative path without the extension, the path-qualified form.
    pub link_path: String,
}

/// The directory tree a profile declares: `top_level` roots, each carrying
/// `fan_out` children for `depth` levels. Position determines identity — no
/// draw is involved, so the tree is the same for every seed and only the
/// documents inside it move.
pub fn directories(shape: &DirShape) -> Vec<String> {
    let roots = shape.top_level.min(TOP_LEVEL_DIRS.len());
    let fan_out = shape.fan_out.min(SUB_DIRS.len());
    let mut all: Vec<String> = TOP_LEVEL_DIRS[..roots]
        .iter()
        .map(|d| (*d).to_string())
        .collect();
    let mut frontier: Vec<String> = all.clone();
    for level in 0..shape.depth {
        if fan_out == 0 {
            break;
        }
        let mut next = Vec::new();
        for parent in &frontier {
            for slot in 0..fan_out {
                // Each level draws a fresh window of the name pool, so a
                // directory never repeats its own parent's name.
                let child = SUB_DIRS[(level * fan_out + slot) % SUB_DIRS.len()];
                let path = format!("{parent}/{child}");
                all.push(path.clone());
                next.push(path);
            }
        }
        frontier = next;
    }
    all
}

/// The lowest of `skew + 1` uniform draws, so a higher skew concentrates
/// placement into the earlier (shallower, more prominent) directories.
fn skewed_index(rng: &mut Rng, len: usize, skew: u32) -> usize {
    let mut best = rng.range(len);
    for _ in 0..skew {
        best = best.min(rng.range(len));
    }
    best
}

fn place(rng: &mut Rng, dirs: &[String], shape: &DirShape, stem: &str) -> (String, String) {
    if dirs.is_empty() || rng.per_mille(shape.root_per_mille) {
        return (format!("{stem}.md"), stem.to_string());
    }
    let dir = &dirs[skewed_index(rng, dirs.len(), shape.placement_skew)];
    (format!("{dir}/{stem}.md"), format!("{dir}/{stem}"))
}

/// Plan every document of `profile`: the ambiguity classes first, then the
/// uniquely-named remainder, then one shuffle so class members are spread
/// through emission order rather than bunched at the front.
pub fn plan(rng: &mut Rng, profile: &Profile, dirs: &[String]) -> Vec<Placed> {
    let mut out: Vec<Placed> = Vec::with_capacity(profile.docs);

    let classes = profile
        .ambiguity
        .classes
        .min(AMBIGUOUS_STEMS.len())
        .min(profile.docs);
    let k = profile.ambiguity.k.min(dirs.len().max(1));
    let mut dir_indices: Vec<usize> = (0..dirs.len()).collect();

    for stem in &AMBIGUOUS_STEMS[..classes] {
        let members = k.min(profile.docs.saturating_sub(out.len()));
        if members == 0 {
            break;
        }
        rng.take_distinct(&mut dir_indices, members);
        for index in &dir_indices[..members] {
            let dir = &dirs[*index];
            out.push(Placed {
                path: format!("{dir}/{stem}.md"),
                stem: (*stem).to_string(),
                link_path: format!("{dir}/{stem}"),
            });
        }
    }

    for i in out.len()..profile.docs {
        let stem = if rng.per_mille(profile.dirs.unicode_stem_per_mille) {
            format!("{}-{i:04}", rng.pick(UNICODE_STEMS))
        } else if rng.per_mille(profile.dirs.spaced_stem_per_mille) {
            format!("{} {i:04}", rng.pick(SPACED_STEMS))
        } else {
            format!("{}-{i:04}", DOC_WORDS[i % DOC_WORDS.len()])
        };
        let (path, link_path) = place(rng, dirs, &profile.dirs, &stem);
        out.push(Placed {
            path,
            stem,
            link_path,
        });
    }

    let count = out.len();
    rng.take_distinct(&mut out, count);
    out
}

/// The realised ambiguity of a plan: how many stems repeat, and the size of
/// the largest class.
pub fn ambiguity_of(plan: &[Placed]) -> (usize, usize) {
    let mut stems: Vec<&str> = plan.iter().map(|p| p.stem.as_str()).collect();
    stems.sort_unstable();
    let mut classes = 0;
    let mut largest = 0;
    let mut run = 0;
    for window in 0..stems.len() {
        run += 1;
        let last = window + 1 == stems.len() || stems[window] != stems[window + 1];
        if last {
            if run > 1 {
                classes += 1;
                largest = largest.max(run);
            }
            run = 0;
        }
    }
    (classes, largest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(top_level: usize, depth: usize, fan_out: usize) -> DirShape {
        DirShape {
            top_level,
            depth,
            fan_out,
            root_per_mille: 0,
            placement_skew: 0,
            unicode_stem_per_mille: 0,
            spaced_stem_per_mille: 0,
        }
    }

    #[test]
    fn directory_count_follows_the_declared_shape() {
        assert_eq!(directories(&shape(3, 0, 4)).len(), 3);
        assert_eq!(directories(&shape(3, 1, 2)).len(), 3 + 6);
        assert_eq!(directories(&shape(10, 2, 3)).len(), 10 * (1 + 3 + 9));
    }

    #[test]
    fn directories_are_seed_independent_and_unique() {
        let dirs = directories(&shape(10, 2, 3));
        let mut sorted = dirs.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), dirs.len(), "a directory is listed twice");
    }

    #[test]
    fn skew_concentrates_placement_into_the_earlier_directories() {
        let mut uniform = Rng::new(4);
        let mut skewed = Rng::new(4);
        let uniform_total: usize = (0..2000).map(|_| skewed_index(&mut uniform, 100, 0)).sum();
        let skewed_total: usize = (0..2000).map(|_| skewed_index(&mut skewed, 100, 3)).sum();
        assert!(
            skewed_total * 2 < uniform_total,
            "skew 3 should crowd the front far harder than skew 0"
        );
    }

    #[test]
    fn ambiguity_classes_land_at_the_declared_size() {
        let profile = Profile::by_name("ambiguous").expect("ambiguous profile");
        let dirs = directories(&profile.dirs);
        let mut rng = Rng::new(11);
        let plan = plan(&mut rng, &profile, &dirs);
        let (classes, largest) = ambiguity_of(&plan);
        assert_eq!(classes, profile.ambiguity.classes);
        assert_eq!(largest, profile.ambiguity.k);
    }

    #[test]
    fn a_plan_holds_the_declared_document_count_at_distinct_paths() {
        for profile in Profile::all() {
            let dirs = directories(&profile.dirs);
            let mut rng = Rng::new(7);
            let plan = plan(&mut rng, profile, &dirs);
            assert_eq!(plan.len(), profile.docs, "{} document count", profile.name);
            let mut paths: Vec<&str> = plan.iter().map(|p| p.path.as_str()).collect();
            paths.sort_unstable();
            let total = paths.len();
            paths.dedup();
            assert_eq!(paths.len(), total, "{} plans a path twice", profile.name);
        }
    }
}
