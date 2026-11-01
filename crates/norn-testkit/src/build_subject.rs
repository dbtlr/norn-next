//! Build provenance for measurements whose bars depend on compiler optimization.

/// Provenance of the package containing the measured executable, supplied by
/// that package's build script (never inferred from testkit's own build).
#[derive(Clone, Copy, Debug)]
pub struct BuildSubject<'a> {
    opt_level: &'a str,
    overrides: &'a str,
}

impl<'a> BuildSubject<'a> {
    pub const fn new(opt_level: &'a str, overrides: &'a str) -> Self {
        Self {
            opt_level,
            overrides,
        }
    }

    /// Check the opt-level-0 subject on which the measurement bars were authored.
    /// Any profile declaration is refused, even one affecting only dependencies.
    /// Build scripts cannot discover Cargo's `--config` arguments or all config
    /// includes; callers must not use those to supply hidden profile overrides.
    pub fn check_unoptimized(self) -> Result<(), String> {
        if self.opt_level == "0" && self.overrides.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "measurement baselines require opt-level=0 with no profile overrides; \
                 measured subject has opt-level={}; profile overrides: {}",
                self.opt_level,
                if self.overrides.is_empty() {
                    "none"
                } else {
                    self.overrides
                }
            ))
        }
    }

    /// Absolute optimized timing bars apply even with debug assertions enabled.
    pub fn is_optimized(self) -> bool {
        matches!(self.opt_level, "1" | "2" | "3" | "s" | "z")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unoptimized_without_overrides_matches_authored_subject() {
        assert_eq!(BuildSubject::new("0", "").check_unoptimized(), Ok(()));
    }

    #[test]
    fn optimization_with_debug_assertions_still_changes_the_subject() {
        // Debug assertions are deliberately absent from the decision's inputs:
        // Cargo keeps them enabled when only test.opt-level changes to 1.
        let error = BuildSubject::new("1", "").check_unoptimized().unwrap_err();
        assert!(error.contains("opt-level=1"), "{error}");
    }

    #[test]
    fn every_override_is_named_even_when_the_package_is_unoptimized() {
        for source in [
            "CARGO_PROFILE_DEV_OPT_LEVEL=0",
            "Cargo.toml: [profile.dev.package.\"*\"]",
            ".cargo/config: [profile.test]",
        ] {
            let error = BuildSubject::new("0", source)
                .check_unoptimized()
                .unwrap_err();
            assert!(error.contains(source), "{error}");
        }
    }

    #[test]
    fn optimized_requires_a_known_nonzero_opt_level() {
        for level in ["1", "2", "3", "s", "z"] {
            assert!(BuildSubject::new(level, "override").is_optimized());
        }
        for level in ["0", "", "unknown"] {
            assert!(!BuildSubject::new(level, "").is_optimized());
        }
    }
}

#[cfg(test)]
#[path = "build_provenance.rs"]
#[allow(dead_code)] // The emitter is used by package build scripts, not this test binary.
mod build_provenance;
