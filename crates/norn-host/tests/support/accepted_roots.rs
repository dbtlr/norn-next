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
/// changed root made after the change began. A first report is not the
/// last, so the wait then passes the watcher's ordered settle barrier
/// (NORN-578): every report the change made is accepted before it returns.
pub struct AcceptedRoots {
    vault: PathBuf,
    normalizer: norn_fs::PathNormalizer,
    authority: Mutex<Option<norn_fs::watch::BuildAuthority>>,
    /// The roots of the last change and the report sequence its wait ended at.
    settled: Mutex<Option<(Vec<norn_fs::NormalizedPath>, u64)>>,
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
            settled: Mutex::new(None),
        }
    }

    pub fn keep(&self, authority: &norn_fs::watch::BuildAuthority) {
        *self.authority.lock().unwrap() = Some(authority.clone());
    }

    /// Make `change`, then wait until the watcher has accepted a report
    /// made since at each of `paths`, which are vault-relative or absolute paths under the vault,
    /// and has ingested every other report the change made.
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
        let reported = wait_until("a report at each changed root", budget, || {
            let (missing, sequence): (Vec<PathBuf>, u64) =
                authority.visit_roots(|sequence, accepted| {
                    let fresh: BTreeSet<norn_fs::NormalizedPath> = accepted
                        .filter(|&(_, reported, _, _)| reported > since)
                        .map(|(root, ..)| root.clone())
                        .collect();
                    let missing = roots
                        .iter()
                        .filter(|root| !fresh.contains(*root))
                        .map(|root| root.as_path().to_path_buf())
                        .collect();
                    (missing, sequence)
                });
            if missing.is_empty() {
                Observed::Met(())
            } else {
                let authority = if authority.held_is_current() {
                    "current"
                } else {
                    "withdrawn"
                };
                Observed::pending(format!(
                    "no report yet at {missing:?}; {} reports accepted since the change, \
                     whose wait began at report {since}; the build's watcher authority is \
                     {authority}",
                    sequence - since
                ))
            }
        });
        if let Err(failure) = reported {
            panic!("{failure}; {}", Self::silence(&authority));
        }
        // One change can report more than once, and the rest can trail the
        // first report at a root by long enough to land as a fresh fact after
        // the build took the change in. Wait until ingest has passed every
        // report made before now.
        authority.settle_changes(budget.work()).unwrap();
        let through = authority.sequence();
        *self.settled.lock().unwrap() = Some((roots, through));
    }

    /// Which silence left a change unreported, for the failing wait to name.
    ///
    /// The wait for a report at each changed root reads only the authority's
    /// accepted roots, so a backend that answered the change with a rescan,
    /// one that went quiet, and one that made no report for this one change
    /// all look alike there (NORN-586). A withdrawn authority names the first.
    /// The ordered settle barrier tells the other two apart: a backend
    /// delivers one subscription's reports in the order they were made, so a
    /// sentinel written now and ingested proves the subscription live and
    /// every report made before it ingested, and one never ingested proves the
    /// subscription delivered nothing more.
    fn silence(authority: &norn_fs::watch::BuildAuthority) -> String {
        let withdrawn = !authority.held_is_current();
        let live = authority.settle_changes(Duration::from_secs(5));
        let delivery = match live {
            Ok(()) => "a settle sentinel written after the wait was ingested, so the \
                       subscription is live and its backend made no report at those roots"
                .to_owned(),
            Err(error) => format!(
                "a settle sentinel written after the wait was not ingested within 5s \
                 ({error:?}), so the subscription delivered nothing more"
            ),
        };
        if withdrawn {
            format!(
                "the build's watcher authority was withdrawn, by a rescan, lost coverage, a \
                 control-file root or the root cap; {delivery}"
            )
        } else {
            delivery
        }
    }

    /// The reports the watcher accepted at the last change's roots after its
    /// wait ended, for a failing assertion to name.
    #[allow(dead_code)] // Named by early_reads; the lib's own tests do not.
    pub fn late_reports(&self) -> String {
        let Some((roots, through)) = self.settled.lock().unwrap().clone() else {
            return "no change made".to_owned();
        };
        let Some(authority) = self.authority.lock().unwrap().clone() else {
            return "no watcher authority".to_owned();
        };
        authority.visit_roots(|sequence, accepted| {
            let late: Vec<String> = accepted
                .filter(|&(root, reported, _, _)| reported > through && roots.contains(root))
                .map(|(root, reported, ..)| format!("{} at {reported}", root.as_path().display()))
                .collect();
            format!(
                "wait ended at report {through}, now {sequence}; late at changed roots: {late:?}"
            )
        })
    }
}
