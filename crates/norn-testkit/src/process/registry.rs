//! Durable registration for development process groups.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::identity::ProcessIdentity;

const DIRECTORY_MODE: u32 = 0o700;
const RECORD_MODE: u32 = 0o600;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Registration {
    pub(super) schema: u32,
    pub(super) run_token: String,
    pub(super) supervisor: ProcessIdentity,
    pub(super) process_group: ProcessIdentity,
    pub(super) registered_at_unix_ms: u64,
    pub(super) deadline_unix_ms: u64,
    pub(super) purpose: String,
    pub(super) state: RegistrationState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum RegistrationState {
    Registered,
}

pub(super) struct Published {
    path: PathBuf,
    directory: PathBuf,
    temporary: Option<PathBuf>,
}

pub(super) struct StoredRegistration {
    pub(super) path: PathBuf,
    pub(super) registration: Registration,
}

#[allow(clippy::disallowed_types)] // The registry owns this machine-local lock handle.
pub(super) struct ReapLock {
    _file: std::fs::File,
}

impl Registration {
    pub(super) fn new(
        purpose: String,
        deadline: Duration,
        registered_at: SystemTime,
        supervisor: ProcessIdentity,
        process_group: ProcessIdentity,
    ) -> io::Result<Self> {
        let registered_at_unix_ms = unix_millis(registered_at)?;
        let deadline_unix_ms = registered_at_unix_ms
            .checked_add(deadline.as_millis().try_into().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "the deadline is too large")
            })?)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the deadline overflows"))?;
        Ok(Registration {
            schema: 1,
            run_token: run_token()?,
            supervisor,
            process_group,
            registered_at_unix_ms,
            deadline_unix_ms,
            purpose,
            state: RegistrationState::Registered,
        })
    }
}

impl Published {
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The registry owns its machine-local files.
    pub(super) fn create(registration: &Registration) -> io::Result<Self> {
        let directory_path = registry_directory()?;
        let directory = std::fs::File::open(&directory_path)?;
        let path = directory_path.join(format!("{}.json", registration.run_token));
        let temporary = directory_path.join(format!(".{}.tmp", registration.run_token));
        let bytes = serde_json::to_vec_pretty(registration).map_err(io::Error::other)?;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(RECORD_MODE)
            .open(&temporary)?;
        let publication = (|| {
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            std::fs::hard_link(&temporary, &path)?;
            Ok::<(), io::Error>(())
        })();
        if let Err(error) = publication {
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }

        if let Err(publication_error) = directory.sync_all() {
            let rollback = remove_if_present(&path)
                .and_then(|()| remove_if_present(&temporary))
                .and_then(|()| directory.sync_all());
            return match rollback {
                Ok(()) => Err(publication_error),
                Err(rollback_error) => Err(io::Error::new(
                    publication_error.kind(),
                    format!(
                        "{publication_error}. Registry publication rollback also failed: {rollback_error}"
                    ),
                )),
            };
        }

        let temporary = match std::fs::remove_file(&temporary) {
            Ok(()) => match directory.sync_all() {
                Ok(()) => None,
                Err(_) => Some(temporary),
            },
            Err(_) => Some(temporary),
        };
        Ok(Published {
            path,
            directory: directory_path,
            temporary,
        })
    }

    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The registry removes the record after proven cleanup.
    pub(super) fn remove(self) -> io::Result<()> {
        let record_removal = remove_if_present(&self.path);
        let temporary_removal = self.temporary.as_deref().map_or(Ok(()), remove_if_present);
        let synchronization = std::fs::File::open(&self.directory).and_then(|file| file.sync_all());
        record_removal.and(temporary_removal).and(synchronization)
    }
}

#[allow(clippy::disallowed_methods)] // The registry owns enumeration of its machine-local records.
pub(super) fn registrations() -> io::Result<Vec<Result<StoredRegistration, String>>> {
    let Some(root) = state_root_if_present()? else {
        return Ok(Vec::new());
    };
    let directory = root.join("process-groups");
    let directory_metadata = match std::fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    validate_directory(&directory, &directory_metadata, DirectoryAccess::OwnerOnly)?;
    let mut paths = std::fs::read_dir(&directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    Ok(paths
        .into_iter()
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .map(|path| read_registration(&path))
        .collect())
}

#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The registry validates and reads cleanup-authorizing files.
fn read_registration(path: &Path) -> Result<StoredRegistration, String> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(format!(
            "{} is not a private owner-held regular file",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let registration: Registration =
        serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    let expected = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if registration.schema != 1 || registration.run_token != expected {
        return Err(format!(
            "{} does not match its schema or run token",
            path.display()
        ));
    }
    Ok(StoredRegistration {
        path: path.to_owned(),
        registration,
    })
}

impl StoredRegistration {
    #[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Removal follows durable audit and is synced by this registry owner.
    pub(super) fn remove(&self) -> io::Result<()> {
        remove_if_present(&self.path)?;
        std::fs::File::open(registry_directory()?)?.sync_all()
    }
}

#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The registry owns the machine-local reaper lock.
pub(super) fn lock_reaper() -> io::Result<ReapLock> {
    let path = state_root()?.join("process-groups.reap.lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(RECORD_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the reaper lock is not a private owner-held regular file",
        ));
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(ReapLock { _file: file })
}

#[allow(clippy::disallowed_methods)] // The registry owns the supplied machine-local path.
fn remove_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn unix_millis(time: SystemTime) -> io::Result<u64> {
    time.duration_since(UNIX_EPOCH)
        .map_err(|error| {
            io::Error::other(format!("the system clock is before Unix time: {error}"))
        })?
        .as_millis()
        .try_into()
        .map_err(|_| io::Error::other("the system clock does not fit the registry format"))
}

#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The registry owns this machine-local directory.
pub(super) fn registry_directory() -> io::Result<PathBuf> {
    let root = state_root()?;
    let registry = root.join("process-groups");
    secure_directory(&registry, DirectoryAccess::OwnerOnly)?;
    Ok(registry)
}

pub(super) fn state_root() -> io::Result<PathBuf> {
    let root = crate::isolation::root();
    secure_directory(&root, DirectoryAccess::OwnerWrites)?;
    Ok(root)
}

#[allow(clippy::disallowed_methods)] // The registry owns validation of its machine-local root.
pub(super) fn state_root_if_present() -> io::Result<Option<PathBuf>> {
    let root = crate::isolation::root();
    let metadata = match std::fs::symlink_metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    validate_directory(&root, &metadata, DirectoryAccess::OwnerWrites)?;
    Ok(Some(root))
}

#[derive(Clone, Copy)]
enum DirectoryAccess {
    OwnerWrites,
    OwnerOnly,
}

#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // The registry creates and validates its owner-only directory.
fn secure_directory(path: &Path, access: DirectoryAccess) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => validate_directory(path, &metadata, access),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(DIRECTORY_MODE);
            match builder.create(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            validate_directory(path, &std::fs::symlink_metadata(path)?, access)
        }
        Err(error) => Err(error),
    }
}

fn validate_directory(
    path: &Path,
    metadata: &std::fs::Metadata,
    access: DirectoryAccess,
) -> io::Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::other(format!(
            "{} is not a real directory",
            path.display()
        )));
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::other(format!(
            "{} is not owned by this user",
            path.display()
        )));
    }
    let forbidden = match access {
        DirectoryAccess::OwnerWrites => 0o022,
        DirectoryAccess::OwnerOnly => 0o077,
    };
    if metadata.mode() & forbidden != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} grants unsafe access to another user", path.display()),
        ));
    }
    Ok(())
}

#[allow(clippy::disallowed_methods, clippy::disallowed_types)] // Reads the operating system's random source for a registry key.
fn run_token() -> io::Result<String> {
    let mut random = std::fs::File::open("/dev/urandom")?;
    let mut bytes = [0_u8; 16];
    random.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
