//! Step 5.1 of the start: the bearer token minted where the file holds none.
//!
//! The host is the one mint site. Its 32 bytes come from the operating
//! system's random source, and they are written create-if-absent under
//! `norn-config`'s `tokens::mutate`, which holds the token file's lock across
//! the read and the replacement: two hosts starting at once mint one token,
//! not two. The log says a token was minted and where; it never carries the
//! secret.

use norn_config::machine::tokens::{self, TokenLabel};
use norn_config::{ConfigDirs, ConfigError};

/// How many random bytes a minted token is.
pub(crate) const SECRET_BYTES: usize = 32;

/// What the mint did.
#[derive(Debug)]
pub(crate) enum Mint {
    /// The file held a token, so none was minted.
    Held,
    /// A `default` token was written down.
    Minted,
    /// A `default` token is in the file, and its durability is not
    /// confirmed: it is stored, and a second one must not be minted.
    MintedUnconfirmed(ConfigError),
}

/// Mint `secret` as the `default` token if the file holds no token.
pub(crate) fn mint(dirs: &ConfigDirs, secret: &[u8; SECRET_BYTES]) -> Result<Mint, ConfigError> {
    let outcome = tokens::mutate(dirs, |tokens| {
        if !tokens.is_empty() {
            return Ok(false);
        }
        tokens.add(&TokenLabel::default_label(), secret)?;
        Ok(true)
    });
    match outcome {
        Ok(true) => Ok(Mint::Minted),
        Ok(false) => Ok(Mint::Held),
        // A refusal after the rename: the token is in the file. Only a write
        // reaches that point, and the only write here is the mint.
        Err(error @ ConfigError::MutationUnconfirmed { .. }) => Ok(Mint::MintedUnconfirmed(error)),
        Err(error) => Err(error),
    }
}

/// 32 bytes from the operating system's random source.
pub(crate) fn random_secret() -> Result<[u8; SECRET_BYTES], getrandom::Error> {
    let mut secret = [0u8; SECRET_BYTES];
    getrandom::fill(&mut secret)?;
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use norn_config::machine::tokens;
    use norn_testkit::scratch::Scratch;

    use super::*;

    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the bases a case's state lives under.
    fn dirs(scratch: &Scratch) -> ConfigDirs {
        std::fs::create_dir_all(scratch.join("config")).expect("a config base");
        ConfigDirs::new(scratch.join("config"), scratch.join("data")).expect("absolute bases")
    }

    #[test]
    fn a_first_mint_writes_default_and_a_second_writes_nothing() {
        let scratch = Scratch::new("norn-serve-mint");
        let dirs = dirs(&scratch);
        let first = random_secret().expect("random bytes");
        assert!(matches!(mint(&dirs, &first), Ok(Mint::Minted)));
        assert!(matches!(mint(&dirs, &[9; SECRET_BYTES]), Ok(Mint::Held)));
        let held = tokens::read(&dirs).expect("a read");
        let labels: Vec<&str> = held.labels().map(TokenLabel::as_str).collect();
        assert_eq!(labels, ["default"]);
        assert_eq!(
            held.get(&TokenLabel::default_label()).unwrap().secret(),
            first
        );
    }

    /// The mint's create-if-absent holds without the channel lock: mints
    /// racing on one config directory leave one token.
    #[test]
    fn racing_mints_leave_one_token() {
        let scratch = Scratch::new("norn-serve-mint-race");
        let dirs = dirs(&scratch);
        let minted: usize = std::thread::scope(|scope| {
            let racers: Vec<_> = (0..8u8)
                .map(|n| {
                    let dirs = &dirs;
                    scope.spawn(move || mint(dirs, &[n; SECRET_BYTES]).expect("a mint"))
                })
                .collect();
            racers
                .into_iter()
                .map(|racer| usize::from(matches!(racer.join().unwrap(), Mint::Minted)))
                .sum()
        });
        assert_eq!(minted, 1);
        assert_eq!(tokens::read(&dirs).expect("a read").labels().count(), 1);
    }

    #[test]
    fn two_draws_differ() {
        assert_ne!(random_secret().unwrap(), random_secret().unwrap());
    }
}
