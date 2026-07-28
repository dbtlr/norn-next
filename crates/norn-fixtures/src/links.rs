//! Wikilink markup for one document: how many, in which form, and how many of
//! them name nothing.

use crate::layout::Placed;
use crate::profile::LinkDensity;
use crate::rng::Rng;
use crate::words::TAILS;

/// The links one document carries.
pub struct Links {
    /// Rendered markup, one entry per link.
    pub markup: Vec<String>,
    /// How many of them name a target that does not exist.
    pub dangling: usize,
    /// Whether this document gathers its links into a trailing list rather
    /// than placing them inline.
    pub trailing_list: bool,
}

/// Draw a document's links against `density`, targeting `plan` and never
/// itself. `index` is the document's own position in the plan; it also seeds
/// the names of the dangling targets, so no two documents invent the same
/// missing document by accident.
pub fn draw(rng: &mut Rng, density: &LinkDensity, plan: &[Placed], index: usize) -> Links {
    let count = rng
        .range(density.mean_per_doc * 2 + 1)
        .min(density.max_per_doc);
    let trailing_list = rng.per_mille(density.trailing_list_per_mille);

    let mut markup = Vec::with_capacity(count);
    let mut dangling = 0;
    for slot in 0..count {
        if plan.len() < 2 || rng.per_mille(density.dangling_per_mille) {
            dangling += 1;
            markup.push(format!("[[absent-{index:05}-{slot}]]"));
            continue;
        }
        let mut target = rng.range(plan.len());
        if target == index {
            target = (target + 1) % plan.len();
        }
        let target = &plan[target];
        let name = if rng.per_mille(density.path_qualified_per_mille) {
            &target.link_path
        } else {
            &target.stem
        };
        if rng.per_mille(density.aliased_per_mille) {
            let display = rng.pick(TAILS);
            markup.push(format!("[[{name}|{display}]]"));
        } else {
            markup.push(format!("[[{name}]]"));
        }
    }

    Links {
        markup,
        dangling,
        trailing_list,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout;
    use crate::profile::Profile;

    fn sample_plan() -> Vec<Placed> {
        let profile = Profile::by_name("small").expect("small profile");
        let dirs = layout::directories(&profile.dirs);
        let mut rng = Rng::new(2);
        layout::plan(&mut rng, &profile, &dirs)
    }

    #[test]
    fn a_link_never_names_its_own_document() {
        let plan = sample_plan();
        let density = Profile::by_name("small").expect("small profile").links;
        let mut rng = Rng::new(19);
        for index in 0..plan.len() {
            let links = draw(&mut rng, &density, &plan, index);
            // The path-qualified form names exactly one document, so a
            // self-link is visible in it. The bare stem cannot carry the
            // assertion: a member of an ambiguity class shares its stem with
            // other documents, and linking that stem names all of them.
            let own_path = format!("[[{}]]", plan[index].link_path);
            for link in &links.markup {
                assert_ne!(link, &own_path, "a document linked its own path");
            }
        }
    }

    #[test]
    fn link_count_stays_within_the_declared_cap() {
        let plan = sample_plan();
        let density = LinkDensity {
            mean_per_doc: 40,
            max_per_doc: 5,
            dangling_per_mille: 0,
            path_qualified_per_mille: 0,
            aliased_per_mille: 0,
            trailing_list_per_mille: 0,
        };
        let mut rng = Rng::new(23);
        for index in 0..64 {
            assert!(draw(&mut rng, &density, &plan, index).markup.len() <= 5);
        }
    }

    #[test]
    fn every_link_is_dangling_at_a_thousand_per_mille() {
        let plan = sample_plan();
        let density = LinkDensity {
            mean_per_doc: 4,
            max_per_doc: 8,
            dangling_per_mille: 1000,
            path_qualified_per_mille: 0,
            aliased_per_mille: 0,
            trailing_list_per_mille: 0,
        };
        let mut rng = Rng::new(29);
        let mut total = 0;
        let mut dangling = 0;
        for index in 0..64 {
            let links = draw(&mut rng, &density, &plan, index);
            total += links.markup.len();
            dangling += links.dangling;
        }
        assert!(total > 0, "the sample drew no links at all");
        assert_eq!(total, dangling);
    }

    #[test]
    fn no_link_is_dangling_at_zero_per_mille() {
        let plan = sample_plan();
        let density = LinkDensity {
            mean_per_doc: 4,
            max_per_doc: 8,
            dangling_per_mille: 0,
            path_qualified_per_mille: 500,
            aliased_per_mille: 500,
            trailing_list_per_mille: 0,
        };
        let mut rng = Rng::new(31);
        for index in 0..64 {
            assert_eq!(draw(&mut rng, &density, &plan, index).dangling, 0);
        }
    }
}
