//! The composition root: the one shipped artifact.
//!
//! Its whole job is to wire the serving side and the client side together —
//! see the crate map in `docs/architecture.md`. `norn-client` parses argv; a
//! `serve` command is handed to `norn-serve`'s entry, which runs the host
//! process in this process. The two crates share no edge, so the parsed
//! arguments cross here, field by field. The build identity the build script
//! captured is handed to the side that reports it.
//!
//! The package is also where the argv corpus, the regression registry and the
//! certification inventory live, because it is the one place cargo makes the
//! built binary reachable from an integration test.

use std::process::ExitCode;

use norn_client::Invocation;

/// `<version>+<short commit>[-dirty]`, or `<version>+unknown`; see `build.rs`.
const BUILD_IDENTITY: &str = env!("NORN_BUILD_IDENTITY");

fn main() -> ExitCode {
    match norn_client::parse(std::env::args_os()) {
        Invocation::Serve(serve) => norn_serve::run(
            BUILD_IDENTITY,
            norn_serve::Serve {
                port: serve.port,
                config: serve.config,
                launch: if serve.auto_launched {
                    norn_serve::Launch::Auto
                } else {
                    norn_serve::Launch::Explicit
                },
            },
        ),
    }
}
