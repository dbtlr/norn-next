//! Build provenance for measurements whose bars depend on compiler optimization.

/// Inputs to the pure measurement-subject decision.
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
    pub fn check_unoptimized(self) -> Result<(), String> {
        if cfg!(debug_assertions) && self.opt_level == "0" && self.overrides.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "measurement baselines require opt-level=0 with no profile overrides; \
                 measured subject has opt-level={}, debug_assertions={}; profile overrides: {}",
                self.opt_level,
                cfg!(debug_assertions),
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
        // Cargo keeps debug assertions enabled when test.opt-level changes to 1.
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
    fn optimized_skip_gate_uses_effective_level_without_certifying_subject() {
        for level in ["1", "2", "3", "s", "z"] {
            assert!(optimized_skip_gate(level, ""));
        }
        assert!(!optimized_skip_gate("0", ""));
        assert!(optimized_skip_gate("0", "-C\x1fopt-level=1"));
        assert!(!optimized_skip_gate("3", "-Copt-level=0"));
        assert_eq!(
            current_build_is_optimized(),
            optimized_skip_gate(opt_level(), env!("NORN_MEASURED_RUSTFLAGS"))
        );
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

mod provenance;

/// Testkit and the measured workspace crates share an opt level unless a
/// per-package override exists; the runtime guard refuses those declarations.
/// Cargo's profile identity reruns this minimal build script on profile changes.
pub fn opt_level() -> &'static str {
    env!("NORN_MEASURED_OPT_LEVEL")
}

/// A skip gate for absolute optimized timing bars, not a subject check.
/// Runs those bars at opt-level 1, 2, 3, s or z (including effective rustflags),
/// regardless of debug assertions; it does not certify profile provenance.
pub fn current_build_is_optimized() -> bool {
    optimized_skip_gate(opt_level(), env!("NORN_MEASURED_RUSTFLAGS"))
}

fn optimized_skip_gate(level: &str, flags: &str) -> bool {
    let effective = provenance::optimization_flag(flags.split('\x1f')).unwrap_or(level);
    BuildSubject::new(effective, "").is_optimized()
}

/// Check the current measurement subject at run time, never at compile time.
/// Cargo CLI `--config` arguments and config includes can hide dependency-only
/// overrides: measurement runs must not use them. Configuration at an invocation
/// directory outside the workspace (with `--manifest-path`) is not seen.
/// The environment and file scan runs at test run time: configuration removed
/// between compilation and a direct binary run is not seen. Relative or unresolved
/// Cargo home is refused because the invoking Cargo directory cannot be recovered.
/// Scan the workspace manifest, nested configs, workspace ancestors and Cargo home.
pub fn check_current_unoptimized() -> Result<(), String> {
    let mut overrides = provenance::current_overrides();
    let flags = env!("NORN_MEASURED_RUSTFLAGS");
    if provenance::optimization_flag(flags.split('\x1f')).is_some() {
        overrides.push_str(&format!(
            "; CARGO_ENCODED_RUSTFLAGS contains optimization flags: {flags:?}"
        ));
    }
    BuildSubject::new(opt_level(), &overrides).check_unoptimized()
}
