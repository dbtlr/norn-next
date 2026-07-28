//! Generation preconditions: what the generator refuses, and what it leaves
//! behind when it succeeds.

mod scratch;

use std::fs;

use norn_fixtures::{Profile, SENTINEL_CONTENT, SENTINEL_FILE, generate};

use scratch::fresh;

fn tiny() -> Profile {
    Profile::by_name("tiny").expect("tiny profile")
}

#[test]
#[allow(clippy::disallowed_methods)] // The case inspects and clears the target it generated into.
fn an_absent_target_is_created() {
    let dir = fresh("pre-absent");
    generate(&tiny(), 1, &dir).expect("generating into an absent directory");
    assert!(dir.join(SENTINEL_FILE).is_file());
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
#[allow(clippy::disallowed_methods)] // The case builds and clears the target it hands the generator.
fn an_empty_target_is_accepted() {
    let dir = fresh("pre-empty");
    fs::create_dir_all(&dir).expect("creating an empty target");
    generate(&tiny(), 1, &dir).expect("generating into an empty directory");
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
#[allow(clippy::disallowed_methods)] // The case seeds and clears the target it hands the generator.
fn a_populated_target_is_refused() {
    let dir = fresh("pre-populated");
    fs::create_dir_all(&dir).expect("creating a target");
    fs::write(dir.join("already-here.md"), b"---\ntitle: Prior\n---\n").expect("seeding a target");
    let error = generate(&tiny(), 1, &dir).expect_err("a populated target must be refused");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(
        dir.join("already-here.md").is_file(),
        "the refusal removed a file it did not write"
    );
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
#[allow(clippy::disallowed_methods)] // The case creates and removes the file it hands the generator.
fn a_target_that_is_a_file_is_refused() {
    let dir = fresh("pre-file");
    fs::write(&dir, b"not a directory").expect("creating a file target");
    let error = generate(&tiny(), 1, &dir).expect_err("a file target must be refused");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    fs::remove_file(&dir).expect("cleanup");
}

#[test]
#[allow(clippy::disallowed_methods)] // The case reads the sentinel out of the tree it generated.
fn the_sentinel_is_written_with_exactly_its_declared_bytes() {
    let dir = fresh("pre-sentinel");
    generate(&tiny(), 1, &dir).expect("generating a tree");
    let bytes = fs::read(dir.join(SENTINEL_FILE)).expect("reading the sentinel");
    assert_eq!(bytes, SENTINEL_CONTENT.as_bytes());
    fs::remove_dir_all(&dir).expect("cleanup");
}
