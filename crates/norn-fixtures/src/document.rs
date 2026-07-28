//! Composing one document: frontmatter, an optional title heading, and the
//! body beneath them.

use crate::dates::from_base_plus_minutes;
use crate::layout::Placed;
use crate::profile::Profile;
use crate::rng::Rng;
use crate::words::{DOC_TYPES, STATUS_VALUES, TAGS};
use crate::yaml;

/// Title case of a file stem: separators become spaces and each word is
/// capitalised, leaving any trailing index in place.
fn title_of(stem: &str) -> String {
    let mut out = String::with_capacity(stem.len());
    for (index, word) in stem.split(['-', '_', ' ']).enumerate() {
        if index > 0 {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// Compose the whole file: frontmatter, optional title heading, then `body`.
pub fn compose(rng: &mut Rng, profile: &Profile, placed: &Placed, body: &str) -> String {
    let title = title_of(&placed.stem);
    let kind = *rng.pick(DOC_TYPES);

    // Timestamps span two years from the fixed base, with `modified` never
    // before `created`. Both come from the seed, never from the clock.
    let created_minutes = rng.range(2 * 365 * 24 * 60) as i64;
    let modified_minutes = created_minutes + rng.range(30 * 24 * 60) as i64;

    let mut front = String::with_capacity(256);
    front.push_str("---\n");
    front.push_str(&format!("title: {}\n", yaml::scalar(&title)));
    front.push_str(&format!("type: {}\n", yaml::scalar(kind)));
    front.push_str(&format!(
        "created: {}\n",
        from_base_plus_minutes(created_minutes).datetime_z()
    ));
    front.push_str(&format!(
        "modified: {}\n",
        from_base_plus_minutes(modified_minutes).datetime_space()
    ));
    if matches!(kind, "task" | "phase") {
        front.push_str(&format!(
            "status: {}\n",
            yaml::scalar(rng.pick(STATUS_VALUES))
        ));
    }
    let tag_count = rng.range(4);
    if tag_count > 0 {
        let mut pool: Vec<&str> = TAGS.to_vec();
        rng.take_distinct(&mut pool, tag_count);
        let tags: Vec<String> = pool[..tag_count].iter().map(|t| yaml::scalar(t)).collect();
        front.push_str(&format!("tags: [{}]\n", tags.join(", ")));
    }
    front.push_str("---\n\n");

    let mut out = front;
    if rng.per_mille(profile.body.title_heading_per_mille) {
        out.push_str(&format!("# {title}\n\n"));
    }
    out.push_str(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout;

    #[test]
    fn titles_capitalise_every_word_of_a_stem() {
        assert_eq!(title_of("sprout-0012"), "Sprout 0012");
        assert_eq!(title_of("Wide Open Spaces 0031"), "Wide Open Spaces 0031");
        assert_eq!(title_of("über-notiz-0004"), "Über Notiz 0004");
        assert_eq!(title_of("index"), "Index");
    }

    #[test]
    fn a_composed_document_opens_and_closes_its_frontmatter() {
        let profile = Profile::by_name("small").expect("small profile");
        let dirs = layout::directories(&profile.dirs);
        let mut rng = Rng::new(3);
        let plan = layout::plan(&mut rng, &profile, &dirs);
        for placed in plan.iter().take(40) {
            let document = compose(&mut rng, &profile, placed, "body text\n");
            assert!(document.starts_with("---\n"));
            assert_eq!(
                document.matches("\n---\n").count(),
                1,
                "frontmatter is not closed exactly once"
            );
            assert!(document.ends_with("body text\n"));
        }
    }

    #[test]
    fn a_status_appears_only_on_the_types_that_carry_one() {
        let profile = Profile::by_name("realistic").expect("realistic profile");
        let dirs = layout::directories(&profile.dirs);
        let mut rng = Rng::new(5);
        let plan = layout::plan(&mut rng, &profile, &dirs);
        for placed in plan.iter().take(300) {
            let document = compose(&mut rng, &profile, placed, "");
            let typed_task =
                document.contains("type: task\n") || document.contains("type: phase\n");
            assert_eq!(
                document.contains("status: "),
                typed_task,
                "status and type disagree in:\n{document}"
            );
        }
    }
}
