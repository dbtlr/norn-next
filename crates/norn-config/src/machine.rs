//! The machine surface: what both sides of the client/host seam read.
//!
//! A client needs two things to talk to a host — where it is listening, and
//! what to authenticate with. The serving side needs the second of them to
//! verify. That is the whole of this module: the loopback [`Endpoint`], and
//! the [`tokens`] file.
//!
//! **The shared dependency is not a channel.** Both sides reading the same
//! convention out of the same crate is what keeps them from drifting; it is
//! not a way for them to talk. Client and host coordinate over loopback HTTP
//! and nowhere else.
//!
//! # The endpoint is a convention, not a discovery
//!
//! There is no endpoint file and no discovery handshake. A channel has one
//! default port, fixed at compile time, and a host listens on it while a
//! client dials it. A dev host and a live host therefore coexist on one
//! machine without either knowing the other exists.

pub mod tokens;

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use crate::Channel;

/// The port a released host listens on.
///
/// Chosen from a block the IANA registry leaves unassigned, and below the
/// ephemeral ranges both Linux and macOS allocate out of, so that a host's
/// port is not one an unrelated process may already have been handed.
pub const LIVE_ENDPOINT_PORT: u16 = 28471;

/// The port a development host listens on.
///
/// Adjacent to [`LIVE_ENDPOINT_PORT`] and never equal to it: the two hosts run
/// side by side on one machine, and a shared port would make the dev client's
/// requests reach live state over a socket even though it cannot reach live
/// state through a path.
pub const DEV_ENDPOINT_PORT: u16 = 28472;

/// Where a host listens, and where a client dials.
///
/// **Loopback only.** The address is not a field: there is no remote host to
/// name, and a value type that could name one would be the first half of
/// remote access arriving without anybody deciding to add it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Endpoint {
    port: u16,
}

impl Endpoint {
    /// The loopback endpoint at `port`.
    pub const fn at(port: u16) -> Self {
        Endpoint { port }
    }

    pub const fn port(self) -> u16 {
        self.port
    }

    /// The socket address to bind or connect to.
    pub const fn address(self) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), self.port)
    }

    /// The origin a request's URL is built on.
    pub fn origin(self) -> String {
        format!("http://{}", self.address())
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.address())
    }
}

/// The endpoint this build's channel uses.
///
/// The only endpoint anything reaches for by default, and it is a function of
/// the compiled channel alone. Nothing takes a channel, so a dev build has no
/// way to ask for the live one.
pub const fn default_endpoint() -> Endpoint {
    match Channel::COMPILED {
        Channel::Dev => Endpoint::at(DEV_ENDPOINT_PORT),
        Channel::Live => Endpoint::at(LIVE_ENDPOINT_PORT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two hosts coexist, which they cannot do on one port.
    #[test]
    fn the_two_channels_listen_on_two_ports() {
        assert_ne!(DEV_ENDPOINT_PORT, LIVE_ENDPOINT_PORT);
    }

    #[test]
    fn an_endpoint_is_loopback() {
        let endpoint = default_endpoint();
        assert_eq!(endpoint.address().ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(
            endpoint.origin(),
            format!("http://127.0.0.1:{}", endpoint.port())
        );
    }

    #[test]
    #[cfg(not(feature = "channel-live"))]
    fn a_dev_build_dials_the_dev_port() {
        assert_eq!(default_endpoint().port(), DEV_ENDPOINT_PORT);
    }

    #[test]
    #[cfg(feature = "channel-live")]
    fn a_live_build_dials_the_live_port() {
        assert_eq!(default_endpoint().port(), LIVE_ENDPOINT_PORT);
    }
}
