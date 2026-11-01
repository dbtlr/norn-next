// Profile changes select a fresh Cargo unit; no configuration files are watched.
#[allow(clippy::disallowed_macros)] // Cargo build-script protocol.
fn main() {
    let level = std::env::var("OPT_LEVEL").expect("Cargo supplies OPT_LEVEL");
    println!("cargo:rustc-env=NORN_MEASURED_OPT_LEVEL={level}");
    let flags =
        std::env::var("CARGO_ENCODED_RUSTFLAGS").expect("Cargo supplies effective rustflags");
    println!("cargo:rustc-env=NORN_MEASURED_RUSTFLAGS={flags}");
    println!("cargo:rerun-if-changed=build.rs");
}
