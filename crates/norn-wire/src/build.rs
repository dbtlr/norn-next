//! The build identity a host and its dialers compare.

use std::fmt;

/// Which build of the `norn` artifact a process is: the package version and
/// the source commit captured when the binary was built, with `-dirty` when the
/// work tree had changes — `0.1.0+3f2a9c1e`, `0.1.0+3f2a9c1e-dirty`, or
/// `0.1.0+unknown` where git was not available.
///
/// The bin's build script captures the string and hands it to the serving and
/// client crates, which each wrap it here, so the two halves of one artifact
/// spell one value. **Two builds are the same build exactly when the strings
/// are equal**; a rebuild at the same commit and dirty state compares equal,
/// and that is accepted.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BuildIdentity(&'static str);

impl BuildIdentity {
    /// Wrap the identity the bin's build script captured.
    pub const fn new(identity: &'static str) -> Self {
        BuildIdentity(identity)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for BuildIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_builds_are_one_build_exactly_when_their_strings_are_equal() {
        assert_eq!(
            BuildIdentity::new("0.1.0+3f2a9c1e"),
            BuildIdentity::new("0.1.0+3f2a9c1e")
        );
        assert_ne!(
            BuildIdentity::new("0.1.0+3f2a9c1e"),
            BuildIdentity::new("0.1.0+3f2a9c1e-dirty")
        );
        assert_eq!(
            BuildIdentity::new("0.1.0+unknown").to_string(),
            "0.1.0+unknown"
        );
    }
}
