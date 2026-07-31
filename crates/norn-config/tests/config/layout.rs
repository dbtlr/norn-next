//! Where everything sits, and the separation that keeps two builds apart.
//!
//! Every path this crate yields is a pure function of the two injected bases
//! and the compiled channel. What the cases here pin is that the function is
//! the one the layout says it is, that nothing is reached or created on the
//! way, and that the channel is in every path rather than in some of them.

use std::path::Path;

use norn_config::{Channel, ConfigDirs, IN_VAULT_CONFIG_PATH, IN_VAULT_SCHEMA_PATH};

use crate::common::{Scratch, name};

/// The channel is not a suffix on one path; it is a directory both trees hang
/// off, so no file of one channel's is a file of the other's.
#[test]
fn every_machine_local_path_sits_under_the_channel_directory() {
    let scratch = Scratch::new("channel");
    let dirs = scratch.dirs();
    let app = Channel::COMPILED.app_directory();

    assert_eq!(dirs.config_dir(), scratch.config_base().join(app));
    assert_eq!(dirs.data_dir(), scratch.data_base().join(app));

    for path in [
        dirs.registry_file(),
        dirs.tokens_file(),
        dirs.schemas_dir(),
        dirs.weights_dir(),
        dirs.vaults_dir(),
        dirs.derived_dir(&name("notes")),
    ] {
        assert!(
            path.components()
                .any(|component| component.as_os_str() == app),
            "`{}` does not sit under `{app}`",
            path.display()
        );
    }
}

/// The other half of the separation: two channels' trees share no path at all,
/// which is what a dev build's inability to reach live state rests on.
#[test]
fn the_two_channels_share_no_directory() {
    assert_ne!(Channel::Dev.app_directory(), Channel::Live.app_directory());

    let scratch = Scratch::new("separation");
    let dirs = scratch.dirs();
    let other = match Channel::COMPILED {
        Channel::Dev => Channel::Live,
        Channel::Live => Channel::Dev,
    };
    let other_tree = scratch.config_base().join(other.app_directory());
    assert!(
        !dirs.config_dir().starts_with(&other_tree),
        "the compiled channel's config directory sits inside the other channel's"
    );
    assert!(
        !other_tree.starts_with(dirs.config_dir()),
        "the other channel's tree sits inside the compiled channel's"
    );
}

#[test]
fn the_config_directory_holds_the_registry_the_tokens_and_the_schemas() {
    let scratch = Scratch::new("config-layout");
    let dirs = scratch.dirs();
    assert_eq!(
        dirs.registry_file(),
        dirs.config_dir().join("registry.toml")
    );
    assert_eq!(dirs.tokens_file(), dirs.config_dir().join("tokens.toml"));
    assert_eq!(dirs.schemas_dir(), dirs.config_dir().join("schemas"));
}

#[test]
fn the_data_directory_holds_the_weights_and_the_per_vault_state() {
    let scratch = Scratch::new("data-layout");
    let dirs = scratch.dirs();
    assert_eq!(dirs.weights_dir(), dirs.data_dir().join("weights"));
    assert_eq!(dirs.vaults_dir(), dirs.data_dir().join("vaults"));
    assert_eq!(
        dirs.derived_dir(&name("notes")),
        dirs.data_dir().join("vaults").join("notes")
    );
}

/// Derived state follows the registered name, so moving a vault's root does
/// not orphan what was derived from it.
#[test]
fn derived_state_is_keyed_by_name_and_never_by_root() {
    let scratch = Scratch::new("derived-key");
    let dirs = scratch.dirs();
    assert_ne!(
        dirs.derived_dir(&name("notes")),
        dirs.derived_dir(&name("work"))
    );
    // The same name yields the same directory whatever else the machine holds.
    assert_eq!(
        dirs.derived_dir(&name("notes")),
        dirs.derived_dir(&name("notes"))
    );
}

/// A path convention is a string, not an act. Asking for one must not create a
/// directory: the first-run janitor and the store's own lifecycle decide when
/// a directory appears, and a getter that made one would make that decision
/// for them.
#[test]
fn asking_for_a_path_creates_nothing() {
    let scratch = Scratch::new("no-creation");
    let dirs = scratch.dirs();
    let _ = dirs.registry_file();
    let _ = dirs.tokens_file();
    let _ = dirs.schemas_dir();
    let _ = dirs.weights_dir();
    let _ = dirs.vaults_dir();
    let _ = dirs.derived_dir(&name("notes"));

    assert_eq!(
        scratch.names_in(&scratch.config_base()),
        Vec::<String>::new(),
        "the config base gained something"
    );
    assert_eq!(
        scratch.names_in(&scratch.data_base()),
        Vec::<String>::new(),
        "the data base gained something"
    );
}

/// The in-vault conventions are relative paths and stay that way: this crate
/// states where the convention puts a file and never joins it to a root or
/// opens it.
#[test]
fn the_in_vault_conventions_are_relative_paths_under_a_dot_directory() {
    assert_eq!(IN_VAULT_SCHEMA_PATH, ".norn/schema.yaml");
    assert_eq!(IN_VAULT_CONFIG_PATH, ".norn/config.yaml");
    for convention in [IN_VAULT_SCHEMA_PATH, IN_VAULT_CONFIG_PATH] {
        assert!(
            Path::new(convention).is_relative(),
            "`{convention}` is not relative to a vault root"
        );
    }
}

/// The value is pure over what it was built from, so two constructions from
/// the same bases are the same value.
#[test]
fn two_dirs_over_the_same_bases_are_equal() {
    let scratch = Scratch::new("purity");
    let again = ConfigDirs::new(scratch.config_base(), scratch.data_base());
    assert_eq!(&again, scratch.dirs());
    assert_eq!(again.channel(), Channel::COMPILED);
}
