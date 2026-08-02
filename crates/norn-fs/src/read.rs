//! Atomic configured-file observation.

use std::path::{Path, PathBuf};

use crate::hash::{ContentHash, read_bytes_and_hash};
use crate::refusal::{Refusal, environment};

/// Bytes and their content hash from one read of one held file descriptor.
#[derive(Clone, Debug)]
pub struct ReadAndHash {
    path: PathBuf,
    bytes: Vec<u8>,
    content_hash: ContentHash,
}

impl ReadAndHash {
    /// The configured path whose file was opened.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The bytes read from the held descriptor.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The content hash computed from exactly [`Self::bytes`].
    pub fn content_hash(&self) -> ContentHash {
        self.content_hash
    }

    /// Splits the observation into parser input and its fingerprint.
    pub fn into_parts(self) -> (Vec<u8>, ContentHash) {
        (self.bytes, self.content_hash)
    }
}

/// Opens `path` once and returns the bytes and hash produced by one read.
///
/// A missing or dangling name, an unreadable file, and a name that does not
/// identify a regular file are environmental refusals naming `path`.
#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // norn-fs owns configured-file access.
pub fn read_and_hash(path: &Path) -> Result<ReadAndHash, Refusal> {
    let mut file = std::fs::File::open(path).map_err(|error| {
        let operation = if error.kind() == std::io::ErrorKind::NotFound
            && std::fs::symlink_metadata(path)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            "resolving dangling symbolic link at"
        } else {
            "opening"
        };
        environment(operation, path, &error)
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| environment("stating", path, &error))?;
    if !metadata.is_file() {
        return Err(environment(
            "reading",
            path,
            &std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the name does not identify a regular file",
            ),
        ));
    }
    let (bytes, content_hash) =
        read_bytes_and_hash(&mut file).map_err(|error| environment("reading", path, &error))?;
    Ok(ReadAndHash {
        path: path.to_owned(),
        bytes,
        content_hash,
    })
}
