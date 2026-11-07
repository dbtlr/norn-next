use norn_testkit::wait::{Budget, Observed, wait_until};
use std::{collections::BTreeSet, path::PathBuf, sync::Mutex, time::Duration};

/// Holds a from-zero build until its watcher has accepted a report at
/// every root a probe changed, so the build's next requeue takes them all.
///
/// A count of accepted reports cannot say which roots reported: one change
/// can report more than once, and two changes can arrive in separate
/// deliveries, the later one after the build reached Ready, where no
/// requeue takes it (NORN-507). The build's own watcher authority names
/// each root it accepted and when, so the wait asks for a report at each
/// changed root made after the change began.
pub struct AcceptedRoots {
    vault: PathBuf,
    normalizer: norn_fs::PathNormalizer,
    authority: Mutex<Option<norn_fs::watch::BuildAuthority>>,
}

impl AcceptedRoots {
    /// An empty vault selects relative paths with case-sensitive identity.
    /// Otherwise, use the same proven case behavior as the vault watcher.
    pub fn new(vault: PathBuf) -> Self {
        let normalizer = if vault.as_os_str().is_empty() {
            norn_fs::PathNormalizer::for_sensitivity(norn_fs::CaseSensitivity::Sensitive)
        } else {
            norn_fs::PathNormalizer::detect(&vault).unwrap()
        };
        AcceptedRoots {
            normalizer,
            vault,
            authority: Mutex::new(None),
        }
    }

    pub fn keep(&self, authority: &norn_fs::watch::BuildAuthority) {
        *self.authority.lock().unwrap() = Some(authority.clone());
    }

    /// Make `change`, then wait until the watcher has accepted a report
    /// made since at each of `paths`, which are vault-relative or absolute paths under the vault.
    pub fn change(&self, paths: &[PathBuf], change: impl FnOnce()) {
        let authority = self
            .authority
            .lock()
            .unwrap()
            .clone()
            .expect("the build hands its watcher authority over before it acts");
        let roots: Vec<norn_fs::NormalizedPath> = paths
            .iter()
            .map(|path| {
                let relative = if path.is_absolute() {
                    path.strip_prefix(&self.vault)
                        .expect("a path under the vault")
                } else {
                    path.as_path()
                };
                self.normalizer.normalize(relative).unwrap()
            })
            .collect();
        let since = authority.visit_roots(|sequence, _| sequence);
        change();
        // A look takes about half a millisecond, but a machine busy with a
        // cargo build can deschedule it far longer: a 10 ms bound failed
        // this wait at a 31.6 ms first look (NORN-509). So the look is
        // bounded as every lifecycle state look here is.
        let budget = Budget::new(Duration::from_secs(20), Duration::from_millis(250));
        wait_until("a report at each changed root", budget, || {
            let missing: Vec<PathBuf> = authority.visit_roots(|_, accepted| {
                let fresh: BTreeSet<norn_fs::NormalizedPath> = accepted
                    .filter(|&(_, reported, _, _)| reported > since)
                    .map(|(root, ..)| root.clone())
                    .collect();
                roots
                    .iter()
                    .filter(|root| !fresh.contains(*root))
                    .map(|root| root.as_path().to_path_buf())
                    .collect()
            });
            if missing.is_empty() {
                Observed::Met(())
            } else {
                Observed::pending(format!("no report yet at {missing:?}"))
            }
        })
        .unwrap();
    }
}
