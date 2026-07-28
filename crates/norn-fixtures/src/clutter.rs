//! Non-Markdown files and the directories that hold nothing at all.

use crate::profile::Clutter;
use crate::rng::Rng;
use crate::words::{BINARY_CLUTTER, CLUTTER_LINES, CLUTTER_STEMS, EMPTY_DIRS, TEXT_CLUTTER};

/// One clutter file: where it goes and what is in it.
pub struct ClutterFile {
    pub path: String,
    pub bytes: Vec<u8>,
}

/// Bytes that are deliberately not valid UTF-8: a plausible magic prefix
/// followed by seeded noise. A tree whose every file decodes as text does not
/// exercise the branch a walk takes when one does not.
fn binary_bytes(rng: &mut Rng, magic: &[u8], size: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(size.max(magic.len()));
    bytes.extend_from_slice(magic);
    while bytes.len() < size {
        bytes.extend_from_slice(&rng.next_u64().to_le_bytes());
    }
    bytes.truncate(size.max(magic.len()));
    bytes
}

fn text_bytes(rng: &mut Rng, size: usize) -> Vec<u8> {
    let mut text = String::with_capacity(size + 64);
    while text.len() < size {
        text.push_str(rng.pick(CLUTTER_LINES));
        text.push('\n');
    }
    text.into_bytes()
}

/// Draw the clutter files a profile asks for, placed across `dirs`.
pub fn files(rng: &mut Rng, clutter: &Clutter, docs: usize, dirs: &[String]) -> Vec<ClutterFile> {
    let count = docs * clutter.files_per_thousand_docs as usize / 1000;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let stem = format!("{}-{i:04}", rng.pick(CLUTTER_STEMS));
        let size = rng.between(200, clutter.max_file_bytes.max(200));
        let (extension, bytes) = if rng.per_mille(clutter.binary_per_mille) {
            let (extension, magic) = *rng.pick(BINARY_CLUTTER);
            (extension, binary_bytes(rng, magic, size))
        } else {
            (*rng.pick(TEXT_CLUTTER), text_bytes(rng, size))
        };
        let path = if dirs.is_empty() {
            format!("{stem}.{extension}")
        } else {
            format!("{}/{stem}.{extension}", rng.pick(dirs))
        };
        out.push(ClutterFile { path, bytes });
    }
    out
}

/// The directories a profile asks for that hold no documents. Position
/// determines identity, so the set is the same for every seed.
pub fn empty_directories(clutter: &Clutter) -> Vec<&'static str> {
    EMPTY_DIRS[..clutter.empty_dirs.min(EMPTY_DIRS.len())].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: Clutter = Clutter {
        files_per_thousand_docs: 200,
        binary_per_mille: 500,
        max_file_bytes: 4_000,
        empty_dirs: 5,
    };

    fn dirs() -> Vec<String> {
        vec!["notes".to_string(), "notes/drafts".to_string()]
    }

    #[test]
    fn file_count_follows_the_rate() {
        let mut rng = Rng::new(3);
        assert_eq!(files(&mut rng, &SAMPLE, 1000, &dirs()).len(), 200);
        assert_eq!(files(&mut rng, &SAMPLE, 50, &dirs()).len(), 10);
    }

    #[test]
    fn paths_are_distinct_and_carry_a_non_markdown_extension() {
        let mut rng = Rng::new(5);
        let emitted = files(&mut rng, &SAMPLE, 2000, &dirs());
        let mut paths: Vec<&str> = emitted.iter().map(|f| f.path.as_str()).collect();
        paths.sort_unstable();
        let total = paths.len();
        paths.dedup();
        assert_eq!(paths.len(), total, "a clutter path repeats");
        for path in paths {
            assert!(!path.ends_with(".md"), "clutter emitted a document: {path}");
        }
    }

    #[test]
    fn both_encodings_appear_and_binary_files_really_are_binary() {
        let mut rng = Rng::new(7);
        let emitted = files(&mut rng, &SAMPLE, 2000, &dirs());
        let binary = emitted
            .iter()
            .filter(|f| String::from_utf8(f.bytes.clone()).is_err())
            .count();
        assert!(binary > 0, "no clutter file is binary");
        assert!(binary < emitted.len(), "every clutter file is binary");
    }

    #[test]
    fn empty_directories_are_capped_at_the_pool() {
        let mut clutter = SAMPLE;
        clutter.empty_dirs = 999;
        assert_eq!(empty_directories(&clutter).len(), EMPTY_DIRS.len());
    }
}
