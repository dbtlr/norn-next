#[path = "performance/timing.rs"]
mod timing;

use timing::GetPrerequisites;

#[test]
fn get_is_available_at_its_own_structure_without_global_content() {
    let mut prerequisites = GetPrerequisites::default();
    assert_eq!(prerequisites.available_ns("requested.md"), None);
    prerequisites.structured("requested.md", 20);
    assert_eq!(prerequisites.available_ns("requested.md"), Some(20));
    prerequisites.structured("other.md", 900);
    assert_eq!(prerequisites.available_ns("requested.md"), Some(20));
}
