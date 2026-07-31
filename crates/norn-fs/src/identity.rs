//! What a landed write reports, and what a lock holds.
//!
//! [`PostState`] is the **post-state identity**: the identity a landed write
//! reports for what it published, by which Norn later recognizes its own
//! writes. It is a named five-field value rather than a passthrough of the
//! operating system's metadata, because each field has one consumer and a
//! passthrough would offer forty and name none of them.
//!
//! [`Identity`] is the smaller half — the `(device, inode)` pair alone — used
//! where there is no content to hash: a lock file's identity is what says the
//! name still resolves to the handle that is held.

use std::fmt;
use std::time::SystemTime;

use crate::hash::ContentHash;

/// The identity a landed write reports for what it published.
///
/// Norn's own writes come back to it as filesystem events, and an event that
/// cannot be recognized as its own is re-derived for nothing. The five fields
/// are what that recognition costs, and each is here for one consumer:
///
/// - [`content_hash`] is the **suppression match** and the own-write ledger's
///   key. It is the only field that concludes: an event whose file hashes to a
///   recorded post-state's hash is a write Norn made.
/// - [`len`] and [`mtime`] are the **cheap re-stat**: a stat is one syscall and
///   these two are what it returns, so a suppression path can rule an event
///   *in* for hashing or leave it alone without reading a byte. They may
///   prioritize; they never conclude.
/// - [`ino`] and [`dev`] are the **ground truth** of which file this is. A
///   rename swaps what a path names, so a path is not an identity; the pair is.
///   It is also what distinguishes a document replaced by Norn from the same
///   path replaced by something else.
///
/// [`content_hash`]: PostState::content_hash
/// [`len`]: PostState::len
/// [`mtime`]: PostState::mtime
/// [`ino`]: PostState::ino
/// [`dev`]: PostState::dev
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PostState {
    /// The SHA-256 of the published bytes. The suppression match, and the only
    /// field that concludes "this is what is there".
    pub content_hash: ContentHash,
    /// How many bytes were published. Half of the cheap re-stat.
    pub len: u64,
    /// When the published file was last modified, as the filesystem recorded
    /// it. The other half of the cheap re-stat, and never a conclusion: two
    /// filesystems keep it at two resolutions and a sync client is free to
    /// carry it forward across a content change.
    pub mtime: SystemTime,
    /// The inode number the published bytes live at. Ground truth for which
    /// file this is, and not a path.
    pub ino: u64,
    /// The device the inode lives on. An inode number is unique per device and
    /// nowhere else, so the pair is the identity and neither half is.
    pub dev: u64,
}

impl PostState {
    /// The `(device, inode)` pair alone.
    pub fn identity(&self) -> Identity {
        Identity {
            dev: self.dev,
            ino: self.ino,
        }
    }
}

/// A file's `(device, inode)` pair: which file it is, whatever it is called.
///
/// Used where there is no content to conclude from — a lock file's bytes are a
/// diagnostic and its identity is the contract.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Identity {
    pub dev: u64,
    pub ino: u64,
}

impl fmt::Display for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Identity { dev, ino } = self;
        write!(f, "device {dev}, inode {ino}")
    }
}

/// The identity of what `metadata` describes.
#[allow(clippy::disallowed_methods)] // The vault filesystem seam: this crate owns vault stat.
pub(crate) fn identity_of(metadata: &std::fs::Metadata) -> Identity {
    use std::os::unix::fs::MetadataExt;
    Identity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    }
}

/// The post-state identity of a file whose content hashed to `content_hash`.
///
/// The length comes from the hashing rather than from the metadata: hashing is
/// the act that read the bytes, so its count is the count of bytes that went
/// into the hash. A size read from a separate stat is a number about a
/// different moment.
#[allow(clippy::disallowed_methods)] // The vault filesystem seam: this crate owns vault stat.
pub(crate) fn post_state(
    content_hash: ContentHash,
    len: u64,
    metadata: &std::fs::Metadata,
) -> PostState {
    let Identity { dev, ino } = identity_of(metadata);
    PostState {
        content_hash,
        len,
        // A filesystem that reports no modification time is one this field
        // cannot describe, and the epoch is the honest reading of "nothing was
        // reported" — it prioritizes nothing and concludes nothing, which is
        // all this field is ever allowed to do.
        mtime: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        ino,
        dev,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_post_state_carries_the_identity_pair() {
        let state = PostState {
            content_hash: ContentHash::of(b"bytes"),
            len: 5,
            mtime: SystemTime::UNIX_EPOCH,
            ino: 7,
            dev: 11,
        };
        assert_eq!(state.identity(), Identity { dev: 11, ino: 7 });
    }

    /// An identity reads as both of its halves. One number alone names no file:
    /// inode 7 exists on every device.
    #[test]
    fn an_identity_names_both_halves() {
        let rendered = Identity { dev: 11, ino: 7 }.to_string();
        assert!(
            rendered.contains("11") && rendered.contains('7'),
            "{rendered}"
        );
    }
}
