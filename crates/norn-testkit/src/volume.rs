//! A mounted HFS+ RAM disk, for the cases that need a volume of their own.
//!
//! A RAM disk is the volume a test can mount and release without privileges,
//! which macOS provides through `hdiutil` and `diskutil`. Elsewhere
//! [`Volume::provision`] answers why it could not, and the case decides
//! whether a missing volume skips it or fails it.
//!
//! The volume is HFS+, so it is case-insensitive and stores every timestamp in
//! whole seconds: two changes made within one second leave mtime and ctime
//! where they were.

use std::path::{Path, PathBuf};

use crate::scratch;

/// One attached and mounted RAM-disk volume, detached when dropped.
pub struct Volume {
    device: String,
    mount: PathBuf,
}

impl Volume {
    /// Attach and mount a fresh volume whose name starts with `norn-{label}`.
    ///
    /// # Errors
    ///
    /// The output of the provisioning command that failed, or why it could
    /// not run.
    pub fn provision(label: &str) -> Result<Self, String> {
        // 65,536 512-byte sectors: 32 MiB, which holds a case's files and the
        // volume's own event log with room to spare.
        let device = command("hdiutil", &["attach", "-nomount", "ram://65536"])?;
        // The device is attached from here on, so the volume owns it before
        // anything else can fail and leave it attached with no owner.
        let mut volume = Self {
            device: device.clone(),
            mount: PathBuf::new(),
        };
        let name = scratch::unique_name(&format!("norn-{label}"));
        command("diskutil", &["eraseVolume", "HFS+", &name, &device])?;
        volume.mount = command("diskutil", &["info", &device])?
            .lines()
            .find_map(|line| line.trim().strip_prefix("Mount Point:").map(str::trim))
            .filter(|mount| !mount.is_empty())
            .ok_or_else(|| format!("{device} reports no mount point"))?
            .into();
        Ok(volume)
    }

    /// Where the volume is mounted.
    #[must_use]
    pub fn mount(&self) -> &Path {
        &self.mount
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        let _ = command("hdiutil", &["detach", "-force", &self.device]);
    }
}

/// Run one provisioning command, reporting its output or why it gave none.
fn command(program: &str, arguments: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| format!("{program} could not run: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
