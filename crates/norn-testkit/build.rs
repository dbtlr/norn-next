// Profile changes select a fresh Cargo unit; no configuration files are watched.
#[allow(clippy::disallowed_macros)] // Cargo build-script protocol.
fn main() {
    let level = std::env::var("OPT_LEVEL").expect("Cargo supplies OPT_LEVEL");
    println!("cargo:rustc-env=NORN_MEASURED_OPT_LEVEL={level}");
    println!("cargo:rerun-if-changed=build.rs");
}
