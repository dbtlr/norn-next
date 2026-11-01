// Source inclusion keeps provenance identical without adding a build dependency
// on testkit (which itself reaches fixtures).
#[path = "../norn-testkit/src/build_provenance.rs"]
mod build_provenance;

fn main() {
    build_provenance::emit();
}
