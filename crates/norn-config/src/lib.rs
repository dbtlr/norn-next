#![forbid(unsafe_code)]
//! Machine-local state, one owner.
//!
//! Everything norn keeps on a machine and outside a vault lives here: where
//! the config and data directories are, the registry of vaults the host
//! serves, the bearer tokens loopback requests carry, the weights directory,
//! and the per-vault derived-state directory.
//!
//! **One write protocol, and it is in this crate.** The registry file and the
//! token file are opened by one module — locking, replacement and mode are
//! decided in a single place, and the paths to those two files are not public,
//! so the API is the only door to them from inside the workspace. What that
//! does not do is stop a process from opening a config-directory path it
//! spells itself; the workspace-wide `std::fs` lint and review are what hold
//! the rest, and the crate map records this crate as the owner of those bytes.
//!
//! **This crate never touches a vault.** It does not read vault content, does
//! not walk a tree, does not watch anything, and holds no opinion about what a
//! vault contains. It knows a vault's *name* and its *root path* because the
//! registry records them; it never opens either.
//!
//! # Where to start
//!
//! - [`ConfigDirs`] — the injected value every other entry point is pure over,
//!   and [`ConfigDirs::from_environment`], the one place the environment is
//!   read.
//! - [`registry`] — the registry file: the vaults a host serves, and the
//!   locked read-modify-write that changes them.
//! - [`machine`] — the surface both sides of the client/host seam share: the
//!   bearer tokens, and the loopback endpoint.
//!
//! # The two surfaces
//!
//! The API splits along the seam boundary invariant 11 draws, and the split is
//! the module structure rather than a convention:
//!
//! - [`registry`] is **host-only**. Registry semantics — which vaults are
//!   served, what attaching one means — are the orchestrator's, and this
//!   module is the storage under them. No client-side path reads it.
//! - [`machine`] is **shared**. A client needs the endpoint to reach the host
//!   and a token to authenticate; the serving side needs the same token to
//!   verify. That is the whole of what both sides read.
//!
//! Nothing from either module is re-exported here. A caller naming a registry
//! type names it through [`registry`], which is what makes "no registry
//! surface outside the orchestrator" a thing a symbol-level rule can say.
//! What lives at the root is what both surfaces are expressed over: the
//! directories, the channel, the layout conventions, the vault name, and the
//! one error type.
//!
//! # Injection
//!
//! The environment is read **once, at the edge**, by
//! [`ConfigDirs::from_environment`]. Every other function in this crate takes
//! the resulting [`ConfigDirs`] and reads no ambient state at all, which is
//! what lets the whole surface be exercised over a temporary directory without
//! a process-wide variable being set.
//!
//! # Channel
//!
//! A build is pinned to a [`Channel`] at compile time, and the channel selects
//! the app-directory name — so a development build's registry, tokens, weights
//! and derived state are a different directory tree from a released build's,
//! and there is **no API that takes a channel**. A dev binary cannot reach
//! live state through a path, because no path it can build names one.
//!
//! The channel also selects the default port
//! ([`machine::default_endpoint`]), and that half is a default rather than a
//! wall: a caller may name any port, which is what the `--port` override is.
//! The guarantee is over machine-local state, not over sockets.

mod document;
mod error;
mod file;

pub mod machine;
pub mod registry;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub use error::ConfigError;
/// The name a vault is keyed by. The grammar is `norn-wire`'s, because a name
/// crosses the client/host seam as well as keying a registry table and naming
/// a directory; this crate reads and writes names under that one grammar
/// rather than a second spelling of it.
pub use norn_wire::VaultName;

/// The vault schema a registry entry falls back to, relative to the vault
/// root.
///
/// A path, not a document: this crate states where the convention puts the
/// file and never opens it. Resolving an entry's schema source — this default
/// or the explicit path the entry names — and reading it belongs to the
/// orchestrator.
pub const IN_VAULT_SCHEMA_PATH: &str = ".norn/schema.yaml";

/// The in-vault config a vault may carry, relative to the vault root.
///
/// Same terms as [`IN_VAULT_SCHEMA_PATH`]: the convention lives here so that
/// two crates do not each decide where it is, and reading it does not.
pub const IN_VAULT_CONFIG_PATH: &str = ".norn/config.yaml";

/// The registry file's name inside the config directory.
const REGISTRY_FILE: &str = "registry.toml";

/// The token file's name inside the config directory.
const TOKENS_FILE: &str = "tokens.toml";

/// The directory under the config directory holding machine-local schema
/// files — the ones a registry entry points at when the vault does not carry
/// its own.
const SCHEMAS_DIRECTORY: &str = "schemas";

/// The directory under the data directory holding fetched embedding weights.
const WEIGHTS_DIRECTORY: &str = "weights";

/// The directory under the data directory holding per-vault derived state.
const VAULTS_DIRECTORY: &str = "vaults";

/// Which build this is, and therefore which machine-local state it may reach.
///
/// The value is fixed at compile time by the `channel-live` feature and read
/// through [`Channel::COMPILED`]. It is a full enum rather than a boolean
/// because both names appear in paths and in ports, and a path built from
/// `if live { .. }` is a path spelled twice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Channel {
    /// A development build. Its state lives under `norn-dev`.
    Dev,
    /// A released build. Its state lives under `norn`.
    Live,
}

impl Channel {
    /// The channel this build is pinned to.
    pub const COMPILED: Channel = if cfg!(feature = "channel-live") {
        Channel::Live
    } else {
        Channel::Dev
    };

    /// The directory name this channel's state lives under, inside both the
    /// config base and the data base.
    ///
    /// This is the whole of the separation: every machine-local path in this
    /// crate is built under one of the two directories the two channels name,
    /// so the two channels share no file at all.
    pub const fn app_directory(self) -> &'static str {
        match self {
            Channel::Dev => "norn-dev",
            Channel::Live => "norn",
        }
    }
}

/// Where this machine keeps norn's state.
///
/// Constructed once, at the edge, and passed down. Every path this value
/// yields is a pure function of the two base directories it was built from and
/// the compiled channel: nothing here reaches the filesystem, and nothing here
/// creates a directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigDirs {
    config: PathBuf,
    data: PathBuf,
}

impl ConfigDirs {
    /// The directories under `config_home` and `data_home`, or a refusal if
    /// either base is not a path machine-local state can live under.
    ///
    /// The two arguments are the **base** directories — the XDG bases, or a
    /// temporary directory in a test. The channel-qualified app directory is
    /// appended here, which is what makes channel separation a property of
    /// construction rather than of every caller remembering it.
    ///
    /// Both bases are absolute and both are UTF-8. A relative base names a
    /// different directory to every process that reads it, and one process
    /// here is a service whose working directory nobody chose.
    pub fn new(
        config_home: impl AsRef<Path>,
        data_home: impl AsRef<Path>,
    ) -> Result<Self, ConfigError> {
        let app = Channel::COMPILED.app_directory();
        let config = absolute_path(config_home.as_ref().to_path_buf(), "config base")?;
        let data = absolute_path(data_home.as_ref().to_path_buf(), "data base")?;
        Ok(ConfigDirs {
            config: config.join(app),
            data: data.join(app),
        })
    }

    /// The directories this machine's environment names.
    ///
    /// **The one place this crate reads the environment.** `XDG_CONFIG_HOME`
    /// and `XDG_DATA_HOME` win when they are set to an absolute path;
    /// otherwise the XDG defaults under `HOME` apply. A relative value is
    /// ignored rather than honoured, as the XDG base directory specification
    /// requires — a relative base means a different directory to every process
    /// that reads it.
    pub fn from_environment() -> Result<Self, ConfigError> {
        let (config_home, data_home) = resolve(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("XDG_DATA_HOME"),
            std::env::var_os("HOME"),
        )?;
        ConfigDirs::new(config_home, data_home)
    }

    /// The config directory: the registry file, the token file and the
    /// machine-local schema files.
    pub fn config_dir(&self) -> &Path {
        &self.config
    }

    /// The data directory: fetched weights and per-vault derived state.
    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    /// Where the registry file sits.
    ///
    /// **In-crate only.** A path to one of the two data files is a way to open
    /// it without the lock, the mode and the replacement protocol that come
    /// with it, so the door to those bytes is [`registry`] and [`machine`] —
    /// which is also what makes "no registry surface outside the orchestrator"
    /// a rule a symbol-level lint can state.
    pub(crate) fn registry_file(&self) -> PathBuf {
        self.config.join(REGISTRY_FILE)
    }

    /// Where the token file sits. In-crate only, on the same terms as
    /// [`ConfigDirs::registry_file`].
    pub(crate) fn tokens_file(&self) -> PathBuf {
        self.config.join(TOKENS_FILE)
    }

    /// Where machine-local schema files sit — the vault schemas kept outside
    /// the vaults they describe.
    pub fn schemas_dir(&self) -> PathBuf {
        self.config.join(SCHEMAS_DIRECTORY)
    }

    /// Where fetched embedding weights sit.
    pub fn weights_dir(&self) -> PathBuf {
        self.data.join(WEIGHTS_DIRECTORY)
    }

    /// Where per-vault derived state sits, all of it.
    pub fn vaults_dir(&self) -> PathBuf {
        self.data.join(VAULTS_DIRECTORY)
    }

    /// Where one vault's derived state sits.
    ///
    /// **Keyed by name, not by root.** Derived state follows the registered
    /// vault rather than the directory it currently points at, so moving a
    /// vault's root does not orphan what was derived from it. The name's
    /// grammar is what makes this a single safe path component.
    ///
    /// A pure path. Nothing is read, nothing is created, and the directory
    /// need not exist.
    pub fn derived_dir(&self, name: &VaultName) -> PathBuf {
        self.vaults_dir().join(name.as_str())
    }

    /// The three names one derived store is keyed by, taken from this value.
    ///
    /// A pure function of the same [`ConfigDirs`] [`ConfigDirs::derived_dir`]
    /// is built from, so a mechanism kept somewhere other than that directory
    /// carries the directory's own identity rather than a second spelling of
    /// it.
    pub fn derived_key(&self, name: &VaultName) -> DerivedKey {
        DerivedKey {
            channel: Channel::COMPILED.app_directory(),
            vault: name.as_str().to_owned(),
            data_base: digest_component(self.data.as_os_str().as_encoded_bytes()),
        }
    }
}

/// The three names one derived store is keyed by: the data base it lives under,
/// the channel this build is pinned to, and the registered vault name.
///
/// These are the three coordinates [`ConfigDirs::derived_dir`]'s path is built
/// from — the data base and the channel through [`ConfigDirs::new`], the name as
/// the last component — and therefore the three coordinates the maintainer lock
/// inside that directory is taken under. Two derived stores are one store
/// exactly when all three agree; a mechanism kept elsewhere is keyed by all
/// three or it is keyed by less than the lock is. The digest is 64-bit and not
/// cryptographic, so two data bases colliding into one key is possible at
/// roughly 2^-64 — the unsafe direction, costing two locks one shared home —
/// and that likelihood is the price accepted for a spellable component.
///
/// The two spellable coordinates are carried as themselves. The data base is a
/// whole absolute path and cannot be one component, so it is carried as a
/// digest (`digest_component`) of it — the one part a reader cannot spell back,
/// which is why the other two are not folded in with it.
///
/// Each part is one path component, which is what makes the key spellable as a
/// directory anywhere: [`Channel::app_directory`] is a fixed name,
/// [`VaultName`]'s grammar admits no separator, and the digest is hexadecimal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivedKey {
    channel: &'static str,
    vault: String,
    data_base: String,
}

impl DerivedKey {
    /// The channel component: the app directory this build's state lives under.
    pub fn channel(&self) -> &str {
        self.channel
    }

    /// The vault component: the registered name the derived directory ends
    /// with.
    pub fn vault(&self) -> &str {
        self.vault.as_str()
    }

    /// The data-base component: which machine-local data directory this store
    /// lives under, as a digest of that directory's path.
    pub fn data_base(&self) -> &str {
        self.data_base.as_str()
    }
}

/// The offset basis of FNV-1a, 64-bit.
///
/// FNV-1a is implemented here rather than reached for because the digest it
/// produces names a directory that outlives the build that made it. The
/// algorithm is fixed by its definition, so this function computes the same
/// bytes in every release. `std::hash::DefaultHasher` is the shape to avoid:
/// its algorithm is explicitly allowed to change between Rust releases, and a
/// changed algorithm would silently rename every home a previous build staged
/// into and orphan what is in them.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// The prime of FNV-1a, 64-bit. Fixed with [`FNV_OFFSET_BASIS`].
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// `bytes` as one filesystem-safe path component: sixteen lowercase hex digits.
///
/// **The bytes are digested as they are held, never canonicalized.** Two
/// spellings of one directory therefore digest to two components and key two
/// homes. That is the safe direction: an extra home costs a directory nobody
/// looks in, where a shared home costs the separation the key exists to buy.
fn digest_component(bytes: &[u8]) -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

/// `path`, if it is a path machine-local state can be expressed over.
///
/// Two demands, and they are one function because every path this crate
/// records or builds under has both. **Absolute**, because a relative path
/// names a different directory to every process that reads it and this state
/// is read by a service, a CLI and a shim in three different ones.
/// **UTF-8**, because a recorded path is written into a TOML file and read
/// back out of one: bytes that are not text do not survive that round trip,
/// and rendering them lossily would report success while writing a different
/// path from the one that was asked for.
///
/// `subject` names which path it is, so the refusal reads as a sentence about
/// the caller's argument rather than about a rule.
pub(crate) fn absolute_path(path: PathBuf, subject: &'static str) -> Result<PathBuf, ConfigError> {
    if path.to_str().is_none() {
        return Err(ConfigError::IllegalPath {
            path,
            subject,
            problem: "a path here is written into a text file and read back, so it is UTF-8",
        });
    }
    if !path.is_absolute() {
        return Err(ConfigError::IllegalPath {
            path,
            subject,
            problem: "a path here is absolute, because it is read by processes whose working \
                      directories differ",
        });
    }
    Ok(path)
}

/// The XDG base directories the given environment names.
///
/// Pure, so that the rules — an absolute XDG value wins, an empty or relative
/// one is ignored, `HOME` is the fallback's only input — are exercised without
/// a process-wide variable being set anywhere.
fn resolve(
    config_home: Option<OsString>,
    data_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<(PathBuf, PathBuf), ConfigError> {
    let home = home.filter(|value| !value.is_empty()).map(PathBuf::from);
    let base = |value: Option<OsString>, fallback: &[&str]| {
        let named = value
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute());
        match named {
            Some(path) => Ok(path),
            None => {
                let home = home.clone().ok_or(ConfigError::Environment {
                    variable: "HOME",
                    problem: "is unset, and it is where the config directory is looked for when \
                              the XDG base directories are not named absolutely",
                })?;
                if !home.is_absolute() {
                    return Err(ConfigError::Environment {
                        variable: "HOME",
                        problem: "is not an absolute path",
                    });
                }
                Ok(fallback.iter().fold(home, |path, part| path.join(part)))
            }
        }
    };
    Ok((
        base(config_home, &[".config"])?,
        base(data_home, &[".local", "share"])?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(text: &str) -> Option<OsString> {
        Some(OsString::from(text))
    }

    #[test]
    fn an_absolute_xdg_base_wins() {
        let (config, data) = resolve(some("/xdg/config"), some("/xdg/data"), some("/home/person"))
            .expect("both bases");
        assert_eq!(config, PathBuf::from("/xdg/config"));
        assert_eq!(data, PathBuf::from("/xdg/data"));
    }

    #[test]
    fn an_unset_xdg_base_falls_back_to_the_home_defaults() {
        let (config, data) = resolve(None, None, some("/home/person")).expect("both bases");
        assert_eq!(config, PathBuf::from("/home/person/.config"));
        assert_eq!(data, PathBuf::from("/home/person/.local/share"));
    }

    /// The specification's rule, and the reason for it: a relative base names
    /// a different directory to every process that reads it.
    #[test]
    fn a_relative_or_empty_xdg_base_is_ignored() {
        let (config, data) =
            resolve(some("relative/config"), some(""), some("/home/person")).expect("both bases");
        assert_eq!(config, PathBuf::from("/home/person/.config"));
        assert_eq!(data, PathBuf::from("/home/person/.local/share"));
    }

    #[test]
    fn an_environment_naming_nothing_is_refused() {
        let error = resolve(None, None, None).expect_err("nowhere to look");
        assert_eq!(
            error,
            ConfigError::Environment {
                variable: "HOME",
                problem: "is unset, and it is where the config directory is looked for when the \
                          XDG base directories are not named absolutely",
            }
        );
    }

    /// One XDG base named absolutely and the other not: the named one wins and
    /// the other still needs `HOME`.
    #[test]
    fn a_half_named_environment_still_needs_home_for_the_other_half() {
        let error =
            resolve(some("/xdg/config"), None, None).expect_err("no home for the data base");
        assert!(matches!(error, ConfigError::Environment { .. }), "{error}");
    }

    /// The key and the derived directory are two spellings of one thing, at the
    /// positions the path actually puts them: the channel is the component
    /// directly under the data base, and the vault name is the last.
    ///
    /// The forbidden shape is asking whether the channel appears *anywhere* in
    /// the path — that passes on a caller whose data base happens to spell
    /// `norn-dev` somewhere, and it cannot see a coordinate the key has left
    /// out.
    #[test]
    fn the_derived_key_names_the_derived_directory_at_its_own_positions() {
        let dirs = ConfigDirs::new("/config", "/data").expect("two bases");
        let name = VaultName::new("notes").expect("a name");
        let key = dirs.derived_key(&name);
        let derived = dirs.derived_dir(&name);

        let components: Vec<_> = derived
            .components()
            .map(|component| component.as_os_str().to_owned())
            .collect();
        assert_eq!(
            derived,
            PathBuf::from("/data")
                .join(key.channel())
                .join(VAULTS_DIRECTORY)
                .join(key.vault()),
            "the derived directory is not the key's own path arithmetic"
        );
        assert_eq!(
            components[2],
            key.channel(),
            "the channel is not the component directly under the data base"
        );
        assert_eq!(
            derived.file_name().expect("a last component"),
            key.vault(),
            "the vault name is not the last component"
        );
        assert_eq!(
            key.data_base(),
            digest_component(dirs.data_dir().as_os_str().as_encoded_bytes()),
            "the data-base part is not the digest of the data directory"
        );
    }

    /// **The bar the third coordinate buys.** Two data bases are two derived
    /// directories, and therefore two keys — every part of them that a
    /// mechanism outside the derived directory is placed by.
    ///
    /// The forbidden shape is a key of channel and name alone. Two hosts
    /// running out of two data bases over one channel and one vault name then
    /// take two locks — one per derived directory — and resolve **one** keyed
    /// fallback home, which is the exact sharing the key exists to prevent.
    #[test]
    fn two_data_bases_key_two_derived_stores() {
        let name = VaultName::new("notes").expect("a name");
        let mine = ConfigDirs::new("/config", "/home/mine/.local/share").expect("two bases");
        let theirs = ConfigDirs::new("/config", "/home/theirs/.local/share").expect("two bases");

        assert_ne!(mine.derived_dir(&name), theirs.derived_dir(&name));
        assert_ne!(
            mine.derived_key(&name),
            theirs.derived_key(&name),
            "two data bases spell one key"
        );
        assert_ne!(
            mine.derived_key(&name).data_base(),
            theirs.derived_key(&name).data_base(),
            "two data bases spell one data-base part"
        );
        assert_eq!(
            mine.derived_key(&name),
            ConfigDirs::new("/config", "/home/mine/.local/share")
                .expect("two bases")
                .derived_key(&name),
            "one data base spelled two keys across two constructions"
        );
    }

    /// The digest is one filesystem-safe component, and it is the digest of the
    /// path as held: two spellings of one directory yield two components, which
    /// errs toward two homes rather than one shared one.
    #[test]
    fn the_data_base_digest_is_one_hex_component_of_the_path_as_held() {
        let digest = digest_component(b"/home/person/.local/share/norn-dev");
        assert_eq!(digest.len(), 16, "{digest} is not sixteen digits");
        assert!(
            digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "{digest} is not lowercase hexadecimal"
        );
        assert_ne!(
            digest,
            digest_component(b"/home/person/.local/share/norn-dev/"),
            "two spellings of one directory digested alike"
        );
        // FNV-1a 64 is fixed by its definition, so these are the bytes every
        // release computes. A change here is a change of algorithm, which
        // orphans every home a previous build staged into.
        assert_eq!(digest_component(b""), "cbf29ce484222325");
        assert_eq!(digest_component(b"a"), "af63dc4c8601ec8c");
    }

    /// The whole of channel separation, at the level it is decided: two
    /// channels, two directory names, and no path built from either can be the
    /// other's.
    #[test]
    fn the_two_channels_name_two_directories() {
        assert_ne!(Channel::Dev.app_directory(), Channel::Live.app_directory());
        assert_eq!(Channel::Dev.app_directory(), "norn-dev");
        assert_eq!(Channel::Live.app_directory(), "norn");
    }

    #[test]
    #[cfg(not(feature = "channel-live"))]
    fn a_build_without_the_feature_is_the_dev_channel() {
        assert_eq!(Channel::COMPILED, Channel::Dev);
    }

    #[test]
    #[cfg(feature = "channel-live")]
    fn a_build_with_the_feature_is_the_live_channel() {
        assert_eq!(Channel::COMPILED, Channel::Live);
    }
}
