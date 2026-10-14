//! Which keys of a vault's field universe a request probably meant.

/// How far, in edits, a candidate may stand from the key a request named.
const NEAREST: usize = 2;

/// How many candidates a report offers.
const OFFERED: usize = 3;

/// The keys of `universe` near `asked`, nearest first.
///
/// **The one rule for a suggestion.** A candidate is a key whose optimal
/// string alignment distance from `asked` is at most two. One edit inserts,
/// removes or replaces one character, or transposes two adjacent characters.
/// Both names are compared with ASCII case folded,
/// so `Status` offers `status` at distance zero. Case outside ASCII is not
/// folded, as the store's case-insensitive path identity does not fold it:
/// `É` and `é` are one edit apart. At most three are offered,
/// ordered by distance and then by the key's own bytes, so the list is the
/// same for the same universe however the universe was enumerated. A key the
/// universe holds twice is offered once.
pub(crate) fn did_you_mean<'a>(
    asked: &str,
    universe: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let asked: Vec<char> = asked.to_ascii_lowercase().chars().collect();
    let mut near: Vec<(usize, &str)> = universe
        .into_iter()
        .filter_map(|key| {
            let folded: Vec<char> = key.to_ascii_lowercase().chars().collect();
            crate::similarity::osa_distance(&asked, &folded, NEAREST)
                .map(|distance| (distance, key))
        })
        .collect();
    near.sort_unstable();
    near.dedup();
    near.into_iter()
        .take(OFFERED)
        .map(|(_, key)| key.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggest(asked: &str, universe: &[&str]) -> Vec<String> {
        did_you_mean(asked, universe.iter().copied())
    }

    /// A candidate stands within two edits; three is too far.
    #[test]
    fn a_candidate_is_at_most_two_edits_away() {
        let universe = ["status", "stat", "sta", "st", "statuses"];
        assert_eq!(suggest("status", &["statu", "stats", "statues"]).len(), 3);
        assert_eq!(suggest("statuz", &universe), ["status", "stat"]);
        assert!(suggest("due", &universe).is_empty());
        // Insertions, removals and replacements each cost one.
        assert_eq!(suggest("tag", &["tags"]), ["tags"]);
        assert_eq!(suggest("tags", &["tag"]), ["tag"]);
        assert_eq!(suggest("tag", &["tug"]), ["tug"]);
        assert!(suggest("tag", &["tagsss"]).is_empty());
    }

    /// ASCII case is folded before the edits are counted, and the key is
    /// offered as the universe spells it. Case outside ASCII is an edit.
    #[test]
    fn ascii_case_is_not_an_edit() {
        assert_eq!(suggest("Status", &["status"]), ["status"]);
        assert_eq!(suggest("due", &["DUE", "Due-date"]), ["DUE"]);
        assert_eq!(suggest("ÉTAT", &["état"]), ["état"], "one edit, at É");
        assert!(
            suggest("ÄÖÜ", &["äöü"]).is_empty(),
            "three letters outside ASCII are three edits"
        );
    }

    /// Nearest first, then by the key's bytes, and three at most.
    #[test]
    fn nearest_first_then_by_key_and_three_at_most() {
        assert_eq!(
            suggest("date", &["dates", "data", "gate", "date2", "Date", "dx"]),
            ["Date", "data", "date2"]
        );
        assert_eq!(
            suggest("ab", &["b", "a", "abc", "x"]),
            ["a", "abc", "b"],
            "ties at one edit order by key"
        );
    }

    /// A universe that names one key twice offers it once.
    #[test]
    fn a_repeated_key_is_offered_once() {
        assert_eq!(suggest("stat", &["status", "status"]), ["status"]);
    }
}
