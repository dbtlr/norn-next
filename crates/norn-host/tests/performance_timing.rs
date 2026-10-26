#[path = "performance/timing.rs"]
mod timing;

use timing::GetPrerequisites;

#[test]
fn pulled_ahead_get_waits_for_content_and_ignores_later_probe_dispatch() {
    let mut prerequisites = GetPrerequisites::default();
    prerequisites.structured("requested.md", 20);
    assert_eq!(prerequisites.available_ns("requested.md"), None);
    prerequisites.content_complete(100);
    assert_eq!(prerequisites.available_ns("requested.md"), Some(100));
    prerequisites.structured("other.md", 900);
    assert_eq!(prerequisites.available_ns("requested.md"), Some(100));
}

#[test]
fn get_waits_for_its_own_structure_when_content_finishes_first() {
    let mut prerequisites = GetPrerequisites::default();
    prerequisites.content_complete(100);
    assert_eq!(prerequisites.available_ns("requested.md"), None);
    prerequisites.structured("requested.md", 150);
    assert_eq!(prerequisites.available_ns("requested.md"), Some(150));
}
