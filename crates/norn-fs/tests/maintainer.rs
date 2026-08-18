//! The maintainer lock's contract suite.
//!
//! A binary of its own, because two of its cases spawn processes and a case that
//! spawns nothing should not wait behind one that does. Exclusion across
//! processes is the claim the lock exists for, and the only construction that
//! can carry it is two real processes: a second call in the same process, or on
//! a second thread, can be excluded by anything at all — including code that
//! reports contention without ever reaching the operating system.
//!
//! # The child
//!
//! The second process is this same test binary, run again with a role in its
//! environment. [`maintainer_role_child`] plays that role and writes what it saw
//! to a file the parent named; in an ordinary run no role is set and it does
//! nothing. A file rather than a stream, because a stream would put this suite in
//! the render seam's business for no gain.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use norn_fs::{
    Acquisition, ContentHash, Incumbent, Maintainership, MaintainershipKey, Precondition, Refusal,
    ShadowHome, move_document, try_acquire, vacate, write,
};
use norn_testkit::process::{Run, RunStatus, Sandbox};
use norn_testkit::scratch;
use norn_testkit::wait::{Budget, Observed, wait_until};

/// Which part the child plays.
const ROLE: &str = "NORN_FS_MAINTAINER_ROLE";
/// The lock file the child works over.
const LOCK: &str = "NORN_FS_MAINTAINER_LOCK";
/// Where the child writes what it saw.
const ANSWER: &str = "NORN_FS_MAINTAINER_ANSWER";
/// The file whose appearance tells a holding child to take its `SIGKILL`.
const DONE: &str = "NORN_FS_MAINTAINER_DONE";

/// The backstop deadline on a holding child: never reached in a healthy run,
/// because the child kills itself the moment the parent says it has seen
/// everything it needs. What this bounds is a wedged run — a child that never
/// acquires, or a parent observation that never lands — so it matches the
/// parent's own wait budget rather than racing it.
const BACKSTOP: Duration = Duration::from_secs(30);

/// How long a child that plays a part and exits may run before it is killed.
/// Reaching it is a wedged child rather than a slow one.
const CHILD_DEADLINE: Duration = Duration::from_secs(30);

/// What the parent waits: far above anything the mechanism should approach, so a
/// failure here is a lock that does not behave rather than a slow machine.
fn budget() -> Budget {
    Budget::new(Duration::from_secs(30), Duration::from_secs(5))
}

/// What the parent waits when one evaluation is a whole child run.
///
/// The probe bound is sized for the slowest evaluation this probe can make — a
/// child that runs to [`CHILD_DEADLINE`] and still reports — so a spawn
/// competing with the rest of the suite reads as the slow spawn it is rather
/// than as a structurally expensive probe. The work bound then has to leave
/// room for such an evaluation *and* for the retries the wait exists to make:
/// a work bound at or below the probe bound spends itself on one wedged child
/// and reports an elapsed wait that never got a second look.
fn spawn_budget() -> Budget {
    Budget::new(
        CHILD_DEADLINE + Duration::from_secs(60),
        CHILD_DEADLINE + Duration::from_secs(5),
    )
}

/// A scratch data directory for the length of one case.
///
/// The naming and the removal are [`scratch::Scratch`]'s; what this adds is
/// where a vault's lock file sits under it.
struct Scratch(scratch::Scratch);

impl Scratch {
    fn new(label: &str) -> Scratch {
        Scratch(scratch::Scratch::new(&format!(
            "norn-fs-maintainer-{label}"
        )))
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    /// Where a vault's lock file sits: inside the norn data directory, keyed by
    /// the vault, with no same-filesystem constraint on it at all.
    fn lock(&self) -> PathBuf {
        self.path("vaults/notes/maintainer.lock")
    }
}

/// The second process, playing whichever part the parent named.
///
/// In an ordinary run there is no role in the environment and this does nothing.
/// It is a test rather than a binary of its own because the artifact under test
/// is this suite's own executable, which is what the parent installs and runs.
#[test]
fn maintainer_role_child() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let lock = PathBuf::from(std::env::var(LOCK).expect("a lock path"));
    let answer = PathBuf::from(std::env::var(ANSWER).expect("an answer path"));

    let acquisition = try_acquire(&lock).expect("an acquisition");
    match (&acquisition, role.as_str()) {
        (Acquisition::Acquired(_), _) => {
            say(&answer, &format!("acquired {}", std::process::id()));
        }
        (Acquisition::Contended { incumbent }, _) => {
            let described = match incumbent {
                Incumbent::Named { pid, version, .. } => format!("contended {pid} {version}"),
                Incumbent::Unknown => "contended unknown".to_string(),
            };
            say(&answer, &described);
            return;
        }
    }
    if role == "hold" {
        // Held until this process dies, which is the whole point: the lock is
        // released by the kernel when the holder ends and by nothing else. The
        // death is a real `SIGKILL`, raised the moment the parent says it has
        // observed everything — uncatchable, so nothing here runs after it and
        // nothing graceful releases the lock.
        let done = PathBuf::from(std::env::var(DONE).expect("a done path"));
        loop {
            #[allow(clippy::disallowed_methods)] // Harness scaffolding: the parent's signal to die.
            let told_to_die = done.exists();
            if told_to_die {
                // SAFETY: raise(SIGKILL) delivers a signal this process cannot
                // catch; the kernel ends it here and nothing returns.
                unsafe {
                    libc::raise(libc::SIGKILL);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// The guard, or a panic naming who has it.
///
/// A case whose subject is not contention says once that it wants the lock. The
/// public surface has no such affordance on purpose: an acquisition is two
/// outcomes and a method that handed back the guard alone would throw away the
/// incumbent every real caller has to report.
///
/// **The wait is what this binary costs, not slack in the lock.** Two cases here
/// spawn child processes, and a child holds a copy of every descriptor this
/// process had open at the moment it was forked until it reaches its `exec` —
/// including a lock descriptor another case is holding, or has just dropped. The
/// kernel releases a `flock` when the last descriptor on it closes, so for the
/// length of somebody else's spawn a lock nothing in this process holds still
/// reads as held, by this process. Every case here works over its own lock file
/// and no caller of this helper contends on purpose — the cases that assert
/// contention reach it through a bare `try_acquire` — so a wait is the honest
/// shape: a caller
/// with a reason to wait declares its own bound, which is exactly what the
/// try-only surface asks of one. A lock that never comes free still fails.
fn take(path: &Path) -> Maintainership {
    wait_until("the lock to be free", budget(), || {
        match try_acquire(path).expect("an acquisition") {
            Acquisition::Acquired(held) => Observed::Met(held),
            Acquisition::Contended { incumbent } => {
                Observed::pending(format!("the lock is held by {incumbent}"))
            }
        }
    })
    .unwrap_or_else(|failure| panic!("{failure}"))
}

/// Write what the child saw, where the parent will look for it.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: the channel between the two processes.
fn say(answer: &Path, what: &str) {
    std::fs::write(answer, what).expect("writing the answer");
}

/// A free lock is taken, and the guard reports the file and the identity it was
/// taken on.
#[test]
fn a_free_lock_is_taken_and_names_what_it_holds() {
    let scratch = Scratch::new("free");
    let path = scratch.lock();
    let held = take(&path);

    assert_eq!(held.path(), path);
    assert_eq!(held.identity(), identity_at(&path));
}

/// **The bar on contention.** A second attempt reports contention as an outcome
/// and names the incumbent from the body the winner stamped.
///
/// The forbidden shape is an error. Contention is the ordinary answer to "may I
/// be the maintainer" and a caller decides what to do about it; a failure would
/// make a busy vault indistinguishable from a broken machine.
#[test]
fn a_contended_lock_names_the_incumbent_rather_than_failing() {
    let scratch = Scratch::new("contended");
    let path = scratch.lock();
    let _held = take(&path);

    let Acquisition::Contended { incumbent } =
        try_acquire(&path).expect("contention is not a failure")
    else {
        panic!("a held lock was taken twice");
    };
    let Incumbent::Named {
        pid,
        version,
        started,
    } = incumbent
    else {
        panic!("the incumbent was not named, so the winner did not stamp the body");
    };
    assert_eq!(pid, std::process::id());
    assert!(!version.is_empty());
    assert!(
        started >= std::time::UNIX_EPOCH,
        "the incumbent's start time reads before the epoch"
    );
}

/// A lock released by dropping its guard is free again, and the lock file is
/// still there.
///
/// **Norn never unlinks a lock file.** Removing it is what manufactures the
/// hazard the identity recheck defends against, and the next acquirer's recheck
/// is only meaningful against a name that stays put.
#[test]
fn a_released_lock_is_free_again_and_its_file_remains() {
    let scratch = Scratch::new("released");
    let path = scratch.lock();
    let identity = {
        let held = take(&path);
        held.identity()
    };

    assert_eq!(
        identity_at(&path),
        identity,
        "releasing the lock removed or replaced its file"
    );
    let again = take(&path);
    assert_eq!(
        again.identity(),
        identity,
        "the second holder is on a different file"
    );
}

/// A lock path whose directory cannot be made is an environmental refusal
/// carrying its kind, which is what makes it distinguishable from contention.
///
/// The forbidden shape is collapsing every failure into contention: a caller
/// would wait for a holder that does not exist, forever, over a path that is
/// wrong.
#[test]
fn a_lock_path_that_cannot_be_prepared_is_an_environmental_refusal() {
    let scratch = Scratch::new("unpreparable");
    // A regular file where a directory belongs.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: arranging an impossible path.
    std::fs::write(scratch.path("vaults"), b"not a directory").expect("a file in the way");

    let refusal = try_acquire(&scratch.lock()).expect_err("a path that cannot hold a lock file");
    let Refusal::Environment { kind, path, .. } = &refusal else {
        panic!("an impossible path was not reported as environmental: {refusal}");
    };
    assert_ne!(
        *kind,
        std::io::ErrorKind::WouldBlock,
        "an impossible path was reported as contention"
    );
    assert!(path.starts_with(scratch.path("vaults")), "{refusal}");
}

/// A symbolic link at the lock's own name is refused rather than followed, so a
/// lock is never taken on a file somewhere else.
///
/// Two holders each locking through a different link would each hold an exclusive
/// lock on a different file and exclude nobody.
#[test]
fn a_symlinked_lock_name_is_refused() {
    let scratch = Scratch::new("symlinked");
    let path = scratch.lock();
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: planting a link at the lock's name.
    {
        std::fs::create_dir_all(path.parent().expect("a directory")).expect("the directory");
        std::os::unix::fs::symlink(scratch.path("elsewhere.lock"), &path).expect("a link");
    }

    let refusal = try_acquire(&path).expect_err("a link at the lock's name");
    assert!(
        matches!(&refusal, Refusal::Environment { operation, .. } if *operation == "opening"),
        "{refusal}"
    );
}

/// **The bar on the inverse guarantee.** A fully contended vault is fully
/// workable: every operation that touches vault bytes proceeds while the
/// maintainer lock is held by somebody else.
///
/// The lock admits one maintainer of *derived state* and restricts nobody's
/// access to files. The strongest form of that is structural — nothing in the
/// write kernel takes a lock path, so there is no argument by which a write could
/// consult one — and this is the live half: with the lock held, a create, a
/// replacement, a removal and a move all run.
#[test]
fn a_contended_vault_is_fully_workable() {
    let scratch = Scratch::new("contended-vault");
    let vault = scratch.path("vault");
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: the vault this case works over.
    std::fs::create_dir_all(&vault).expect("a vault root");
    let key = MaintainershipKey::new("norn-dev", "notes", "0123456789abcdef")
        .expect("three path components");
    let shadows = ShadowHome::resolve(&vault, &scratch.path("vaults/notes/tmp"), &key)
        .expect("a shadow home");

    let _held = take(&scratch.lock());
    // And it is genuinely held: a second attempt is contended.
    assert!(
        matches!(
            try_acquire(&scratch.lock()).expect("an attempt"),
            Acquisition::Contended { .. }
        ),
        "the lock is not held, so nothing here is contended"
    );

    let created = vault.join("fresh.md");
    write(&created, b"one", Precondition::Create, &shadows).expect("a create under a held lock");
    write(
        &created,
        b"two",
        Precondition::Replace(ContentHash::of(b"one")),
        &shadows,
    )
    .expect("a replacement under a held lock");
    let moved = vault.join("moved.md");
    move_document(&created, &moved, ContentHash::of(b"two"), &shadows)
        .expect("a move under a held lock");
    vacate(&moved, ContentHash::of(b"two")).expect("a removal under a held lock");
}

/// **The bar on foreign interference, through the public surface.** A waiter
/// retrying while something outside the lifecycle unlinks the lock file and puts
/// a new one at its name ends up holding the file the name means, not the one
/// that was taken away.
///
/// The construction is the one the try-only shape allows: a waiter cannot park
/// inside the primitive, so it polls under a bound it declared, and the
/// interference happens while it is provably still waiting — this thread holds the
/// lock across the unlink-and-recreate and only then lets go. The disruptive actor
/// is deliberately foreign: nothing in this crate ever removes a lock file, so
/// this condition only ever arises from outside.
///
/// Both interleavings converge, which is why the assertion is about where the
/// waiter *ends up* rather than about how it got there. A waiter that opens the
/// replacement takes it straight away; one that opens the doomed file and locks it
/// after this thread lets go finds the name pointing elsewhere and drops it.
///
/// What the retry defends against at the exact instant the hazard opens, and what
/// the bound costs when interference never stops, are checked inside the crate,
/// where that instant can be reached deliberately.
#[test]
fn a_waiter_converges_on_the_lock_file_the_name_means() {
    let scratch = Scratch::new("foreign-churn");
    let path = scratch.lock();
    let held = take(&path);
    let doomed = held.identity();

    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    std::thread::scope(|scope| {
        let waiter = {
            let attempts = std::sync::Arc::clone(&attempts);
            let path = path.clone();
            scope.spawn(move || {
                wait_until("the lock to come free", budget(), || {
                    attempts.fetch_add(1, Ordering::Relaxed);
                    match try_acquire(&path) {
                        Ok(Acquisition::Acquired(held)) => Observed::Met(held.identity()),
                        Ok(Acquisition::Contended { incumbent }) => {
                            Observed::pending(format!("held by {incumbent}"))
                        }
                        // An attempt that reaches the lock's name in the instant
                        // it does not resolve is not an answer and is not a
                        // failure of the lock. This is what the retry bound in a
                        // caller is for, and asking again is what it does.
                        Err(refusal) => Observed::pending(format!("the attempt met {refusal}")),
                    }
                })
                .expect("a lock that comes free")
            })
        };

        // The waiter is provably still waiting: this thread holds the lock.
        wait_until("the waiter to be waiting", budget(), || {
            match attempts.load(Ordering::Relaxed) {
                0 => Observed::pending("the waiter has not tried yet"),
                tried => Observed::Met(tried),
            }
        })
        .expect("the waiter to make an attempt");

        // A foreign actor takes the lock's name away and puts a different file at
        // it. This thread's lock is now on an inode nothing resolves to.
        #[allow(clippy::disallowed_methods)] // Harness scaffolding: playing the foreign actor.
        {
            std::fs::remove_file(&path).expect("removing the lock file");
            std::fs::write(&path, b"").expect("a replacement lock file");
        }
        let replacement = identity_at(&path);
        assert_ne!(replacement, doomed, "the replacement is the same file");

        drop(held);
        let settled = waiter.join().expect("the waiter");
        assert_eq!(
            settled, replacement,
            "the waiter settled for the file that was taken away from under it"
        );
        assert_eq!(
            settled,
            identity_at(&path),
            "the waiter holds a file the name does not resolve to"
        );
    });
}

/// **The bar on cross-process exclusion, from the loser's side.** A second
/// *process* is excluded and names the incumbent; once the lock is released it
/// takes it.
///
/// A second attempt in one process, or on a second thread, proves nothing about
/// the operating system: an implementation that returned contention without ever
/// reaching it would pass. Two processes cannot be excluded by anything but the
/// real primitive.
#[test]
fn a_second_process_is_excluded_and_names_the_incumbent() {
    let scratch = Scratch::new("cross-process");
    let sandbox = Sandbox::new(&std::env::temp_dir(), "norn-fs-maintainer").expect("a sandbox");
    let child = installed_child(&sandbox);
    let lock = scratch.lock();
    let answer = scratch.path("answer");

    let held = take(&lock);

    run_child(&sandbox, &child, "try", &lock, &answer).assert_success();
    assert_eq!(
        read_answer(&answer),
        format!("contended {} {}", std::process::id(), version()),
        "a second process was not excluded, or did not name the incumbent"
    );

    drop(held);
    // A single-shot attempt here would report a copy of this process's own
    // descriptor as the incumbent: a sibling case that forked while the lock
    // above was open carries that descriptor until its `exec`, and the lock
    // stays taken for as long as the copy lives. So the second process asks
    // again under the bound this case declares, one whole child run per look.
    let said = wait_until(
        "a released lock to be free to a second process",
        spawn_budget(),
        || {
            run_child(&sandbox, &child, "try", &lock, &answer).assert_success();
            match read_answer(&answer) {
                said if said.starts_with("acquired ") => Observed::Met(said),
                said => Observed::pending(format!("the second process said {said:?}")),
            }
        },
    )
    .unwrap_or_else(|failure| panic!("{failure}"));
    assert_ne!(
        said,
        format!("acquired {}", std::process::id()),
        "the answer is this process's, so the child did not write it"
    );
}

/// **The bar on release by death.** A maintainer that is killed releases its
/// lock, and nothing had to unlink anything for that to happen.
///
/// This is why there is no lock timeout and no way to break a lock: a lock still
/// held is a process still alive, so an age threshold could only ever take one
/// away from a live holder. The child ends with `SIGKILL`, which it cannot
/// catch, so nothing it does releases the lock — the kernel does. The kill fires
/// when the parent says it has observed the exclusion, so the case runs as fast
/// as its observations rather than waiting out a clock, and a slow spawn only
/// delays it.
///
/// Both directions of the exclusion are observed here: the parent is excluded
/// while the child holds, and the parent succeeds once the child is gone. Each is
/// waited for under a bound this case declared.
#[test]
fn a_killed_maintainer_releases_its_lock() {
    let scratch = Scratch::new("killed");
    let sandbox = Sandbox::new(&std::env::temp_dir(), "norn-fs-killed").expect("a sandbox");
    let child = installed_child(&sandbox);
    let lock = scratch.lock();
    let answer = scratch.path("answer");
    let done = scratch.path("done");

    let (sender, receiver) = std::sync::mpsc::channel();
    let held_by = {
        let sandbox = &sandbox;
        let child = &child;
        let lock = &lock;
        let answer = &answer;
        let done = &done;
        std::thread::scope(|scope| {
            scope.spawn(move || {
                let outcome = Run::new(sandbox, child)
                    .args(["--exact", "maintainer_role_child", "--test-threads=1"])
                    .env(ROLE, "hold")
                    .env(LOCK, lock)
                    .env(ANSWER, answer)
                    .env(DONE, done)
                    .deadline(BACKSTOP)
                    .wait();
                let _ = sender.send(outcome);
            });

            // The child says so once it holds the lock.
            let said = wait_until(
                "the child to hold the lock",
                budget(),
                || match try_read_answer(answer) {
                    Ok(said) if said.starts_with("acquired ") => Observed::Met(said),
                    Ok(said) => Observed::pending(format!("the child said {said:?}")),
                    Err(error) => Observed::pending(format!("no answer yet: {error}")),
                },
            )
            .expect("the child to take a free lock");
            let held_by: u32 = said
                .trim()
                .rsplit(' ')
                .next()
                .expect("a process id")
                .parse()
                .expect("a process id");

            // This process is excluded while the child is alive, and the body it
            // stamped names it.
            let incumbent =
                wait_until(
                    "this process to be excluded",
                    budget(),
                    || match try_acquire(lock).expect("an attempt") {
                        Acquisition::Contended { incumbent } => Observed::Met(incumbent),
                        Acquisition::Acquired(_) => {
                            Observed::pending("the lock was free while the child held it")
                        }
                    },
                )
                .expect("a lock held by another process to be contended");
            let Incumbent::Named { pid, version, .. } = &incumbent else {
                panic!("a lock held by another process named no incumbent");
            };
            assert_eq!(
                *pid, held_by,
                "the contended outcome does not name the child that holds the lock"
            );
            assert_eq!(*version, self::version());

            // Everything observable while the child lives has been observed, so
            // it may die now.
            say(done, "die");

            // And free once the child is killed.
            wait_until(
                "the killed child's lock to be free",
                budget(),
                || match try_acquire(lock).expect("an attempt") {
                    Acquisition::Acquired(held) => Observed::Met(held),
                    Acquisition::Contended { incumbent } => {
                        Observed::pending(format!("still held by {incumbent}"))
                    }
                },
            )
            .expect("a dead maintainer's lock to be free");
            held_by
        })
    };

    let outcome = receiver
        .recv_timeout(Duration::from_secs(30))
        .expect("the child's run to report")
        .expect("waiting on the child");
    assert_eq!(
        outcome.status,
        RunStatus::Signaled(libc::SIGKILL),
        "the child ended some other way, so the release was not caused by a kill"
    );
    assert_ne!(held_by, std::process::id());

    // The lock file survived every one of it, because nothing unlinks one.
    assert!(exists(&lock), "the lock file was removed");
}

/// This suite's own executable, installed inside `sandbox`.
///
/// The artifact is copied rather than run where cargo put it: a concurrent build
/// is entitled to rewrite it, and executing a file while it is being written is a
/// failure of the harness rather than of the program.
fn installed_child(sandbox: &Sandbox) -> PathBuf {
    let source = std::env::current_exe().expect("this suite's own executable");
    sandbox
        .install_binary(&source)
        .expect("installing this suite's executable")
}

/// Run the child in `role` and wait for it.
fn run_child(
    sandbox: &Sandbox,
    child: &Path,
    role: &str,
    lock: &Path,
    answer: &Path,
) -> norn_testkit::process::Outcome {
    Run::new(sandbox, child)
        .args(["--exact", "maintainer_role_child", "--test-threads=1"])
        .env(ROLE, role)
        .env(LOCK, lock)
        .env(ANSWER, answer)
        .deadline(CHILD_DEADLINE)
        .wait()
        .expect("running the child")
}

/// What the child has said so far, or why nothing has been read yet.
#[allow(clippy::disallowed_methods)] // Harness scaffolding: the channel between the two processes.
fn try_read_answer(answer: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(answer)
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: the channel between the two processes.
fn read_answer(answer: &Path) -> String {
    std::fs::read_to_string(answer)
        .unwrap_or_else(|e| panic!("reading {}: {e}", answer.display()))
        .trim()
        .to_string()
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: judging which file a name means.
fn identity_at(path: &Path) -> norn_fs::Identity {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("reading the identity of {}: {e}", path.display()));
    norn_fs::Identity {
        dev: metadata.dev(),
        ino: metadata.ino(),
    }
}

#[allow(clippy::disallowed_methods)] // Harness scaffolding: judging what a case left behind.
fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// The version a winner stamps, which is this build's.
fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
