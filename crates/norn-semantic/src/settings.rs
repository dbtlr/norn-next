//! What the engine's config section means. The host dispatches the raw
//! `[engine.semantic]` table and defines no behavior past the handoff; this
//! module is the engine owning its section's meaning, defaults, and
//! validation.
//!
//! Delivery is the enable act: a section that parses enables the engine, an
//! absent section disables it, and a section this module refuses puts the
//! engine into a typed refused state of its own — never an error the vault's
//! lane-1 lifecycle sees.

/// The engine's per-vault settings, read from its section.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Settings {
    /// Whether the engine runs for this vault. Present-and-true by default:
    /// writing the section is the enablement, and `enabled = false` is the
    /// spelling that keeps the section (and whatever else it carries) while
    /// switching the engine off.
    pub enabled: bool,
}

/// A section this engine refuses to run under.
///
/// Unknown keys are ignored, the posture the config envelope holds at every
/// level: a key this build does not read may be a later build's. A key this
/// build does read at the wrong type is refused, because guessing either way
/// silently inverts the author's intent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SectionError {
    pub what: String,
}

impl std::fmt::Display for SectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the engine section is refused: {}", self.what)
    }
}

impl std::error::Error for SectionError {}

impl Settings {
    /// The meaning of one delivered `[engine.semantic]` table.
    pub fn from_section(section: &toml::Table) -> Result<Settings, SectionError> {
        let enabled = match section.get("enabled") {
            None => true,
            Some(toml::Value::Boolean(enabled)) => *enabled,
            Some(other) => {
                return Err(SectionError {
                    what: format!(
                        "`enabled` holds a {}, and this engine reads it as a boolean",
                        other.type_str()
                    ),
                });
            }
        };
        Ok(Settings { enabled })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_section_enables_and_unknown_keys_are_a_later_builds() {
        let section: toml::Table = toml::from_str("chunking = \"none\"\n").unwrap();
        assert_eq!(
            Settings::from_section(&section),
            Ok(Settings { enabled: true })
        );
    }

    #[test]
    fn enabled_false_keeps_the_section_and_switches_the_engine_off() {
        let section: toml::Table = toml::from_str("enabled = false\n").unwrap();
        assert_eq!(
            Settings::from_section(&section),
            Ok(Settings { enabled: false })
        );
    }

    #[test]
    fn a_mistyped_enabled_is_refused_with_the_type_it_held() {
        let section: toml::Table = toml::from_str("enabled = \"yes\"\n").unwrap();
        let error = Settings::from_section(&section).unwrap_err();
        assert!(error.what.contains("string"), "{error}");
    }
}
