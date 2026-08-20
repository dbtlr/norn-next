#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use norn_testkit::scratch::Scratch;

#[test]
#[allow(clippy::disallowed_methods)] // The integration seam observes the process registry and workload marker.
fn registration_exists_before_the_workload_starts() {
    let scratch = Scratch::new("norn-process-registration-first");
    let state = scratch.join("state");
    let observed = scratch.join("observed.json");
    let script = format!(
        "record=$(find \"$NORN_TEST_ISOLATION_DIR/process-groups\" -name '*.json' -type f -print -quit) && \
         test -n \"$record\" && cp \"$record\" '{}'",
        observed.display()
    );

    let output = Command::new(env!("CARGO_BIN_EXE_norn-process"))
        .args([
            "supervise",
            "--purpose",
            "registration-first-test",
            "--deadline-seconds",
            "30",
            "--",
            "/bin/sh",
            "-c",
            &script,
        ])
        .env("NORN_TEST_ISOLATION_DIR", &state)
        .output()
        .expect("running norn-process");

    assert!(
        output.status.success(),
        "the supervisor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record: serde_json::Value = serde_json::from_slice(
        &fs::read(&observed).expect("the workload's copy of its registry record"),
    )
    .expect("a JSON registry record");
    assert_eq!(record["schema"], 1);
    assert_eq!(record["purpose"], "registration-first-test");
    assert_eq!(record["state"], "registered");
    assert_eq!(record["run_token"].as_str().map(str::len), Some(32));
    assert!(
        record["supervisor"]["pid"]
            .as_i64()
            .is_some_and(|pid| pid > 0)
    );
    assert!(
        record["process_group"]["pid"]
            .as_i64()
            .is_some_and(|pid| pid > 0)
    );
    assert!(
        record["supervisor"]["start"]["value"]
            .as_u64()
            .is_some_and(|start| start > 0)
    );
    assert!(
        record["process_group"]["start"]["value"]
            .as_u64()
            .is_some_and(|start| start > 0)
    );
    assert!(
        record["deadline_unix_ms"].as_u64().unwrap()
            > record["registered_at_unix_ms"].as_u64().unwrap()
    );
    #[cfg(target_os = "linux")]
    {
        assert_eq!(
            record["process_group"]["start"]["source"],
            "linux-clock-ticks"
        );
        assert!(
            record["process_group"]["start"]["boot_id"]
                .as_str()
                .is_some_and(|boot_id| !boot_id.is_empty())
        );
    }
    #[cfg(target_os = "macos")]
    assert_eq!(
        record["process_group"]["start"]["source"],
        "darwin-microseconds"
    );
    for subject in ["supervisor", "process_group"] {
        assert!(
            record[subject]["start"]["boot_id"]
                .as_str()
                .is_some_and(|boot_id| !boot_id.is_empty())
        );
    }
    assert_registry_empty(&state);
}

#[test]
#[allow(clippy::disallowed_methods)] // The integration seam checks the launcher after the direct workload has exited.
fn the_launcher_pins_the_registered_group_after_the_workload_exits() {
    let scratch = Scratch::new("norn-process-resident-launcher");
    let state = scratch.join("state");
    let ready = scratch.join("ready");
    let release = scratch.join("release");
    let workload_pid = scratch.join("workload-pid");
    let script = format!(
        "echo $$ > '{}' && touch '{}' && while [ ! -e '{}' ]; do sleep 0.01; done",
        workload_pid.display(),
        ready.display(),
        release.display()
    );
    let mut supervisor = Command::new(env!("CARGO_BIN_EXE_norn-process"))
        .args([
            "supervise",
            "--purpose",
            "resident-launcher-test",
            "--deadline-seconds",
            "30",
            "--",
            "/bin/sh",
            "-c",
            &script,
        ])
        .env("NORN_TEST_ISOLATION_DIR", &state)
        .spawn()
        .expect("spawning norn-process");

    wait_for_file(&ready);
    let record_path = one_registry_record(&state);
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(&record_path).expect("the live registry record"))
            .expect("a JSON registry record");
    let pgid = record["process_group"]["pid"].as_i64().unwrap() as libc::pid_t;
    let workload: libc::pid_t = fs::read_to_string(&workload_pid)
        .expect("the workload pid")
        .trim()
        .parse()
        .expect("a numeric workload pid");

    assert_eq!(
        unsafe { libc::kill(supervisor.id() as libc::pid_t, libc::SIGSTOP) },
        0
    );
    fs::write(&release, b"release").expect("releasing the workload");
    wait_for_process_absence(workload);
    assert_eq!(
        unsafe { libc::kill(pgid, 0) },
        0,
        "the leader was not resident"
    );
    assert_eq!(
        unsafe { libc::kill(supervisor.id() as libc::pid_t, libc::SIGKILL) },
        0
    );
    assert_eq!(
        supervisor
            .wait()
            .expect("waiting for norn-process")
            .signal(),
        Some(libc::SIGKILL)
    );
    assert_eq!(unsafe { libc::kill(pgid, 0) }, 0);
    assert!(record_path.exists());

    assert_eq!(unsafe { libc::killpg(pgid, libc::SIGKILL) }, 0);
    wait_for_group_absence(pgid);
    fs::remove_file(record_path).expect("removing the controlled fixture record");
    assert_registry_empty(&state);
}

#[test]
#[allow(clippy::disallowed_methods)] // The integration seam queues both termination signals and observes the selected one.
fn a_second_pending_signal_does_not_replace_the_selected_signal() {
    let scratch = Scratch::new("norn-process-two-signals");
    let state = scratch.join("state");
    let ready = scratch.join("ready");
    let mut supervisor = Command::new(env!("CARGO_BIN_EXE_norn-process"))
        .args([
            "supervise",
            "--purpose",
            "two-signals-test",
            "--deadline-seconds",
            "30",
            "--",
            "/bin/sh",
            "-c",
            &format!("touch '{}' && exec sleep 3600", ready.display()),
        ])
        .env("NORN_TEST_ISOLATION_DIR", &state)
        .spawn()
        .expect("spawning norn-process");

    wait_for_file(&ready);
    let pid = supervisor.id() as libc::pid_t;
    assert_eq!(unsafe { libc::kill(pid, libc::SIGSTOP) }, 0);
    assert_eq!(unsafe { libc::kill(pid, libc::SIGINT) }, 0);
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    assert_eq!(unsafe { libc::kill(pid, libc::SIGCONT) }, 0);

    assert_eq!(
        supervisor
            .wait()
            .expect("waiting for norn-process")
            .signal(),
        Some(libc::SIGINT)
    );
    assert_registry_empty(&state);
}

#[test]
#[allow(clippy::disallowed_methods)] // The integration seam supplies one inheritable descriptor and observes the workload boundary.
fn the_workload_inherits_no_unrelated_file_descriptor() {
    let scratch = Scratch::new("norn-process-file-descriptor");
    let state = scratch.join("state");
    let mut descriptors = [-1; 2];
    assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
    let observed_fd = descriptors[0];
    assert_ne!(unsafe { libc::fcntl(observed_fd, libc::F_SETFD, 0) }, -1);

    let status = Command::new(env!("CARGO_BIN_EXE_norn-process"))
        .args([
            "supervise",
            "--purpose",
            "file-descriptor-test",
            "--deadline-seconds",
            "30",
            "--",
            "/bin/sh",
            "-c",
            &format!("test ! -e /dev/fd/{observed_fd}"),
        ])
        .env("NORN_TEST_ISOLATION_DIR", &state)
        .status()
        .expect("running norn-process");
    assert_eq!(unsafe { libc::close(descriptors[0]) }, 0);
    assert_eq!(unsafe { libc::close(descriptors[1]) }, 0);

    assert!(status.success());
    assert_registry_empty(&state);
}

#[test]
#[allow(clippy::disallowed_methods)] // The integration seam arranges an unusable registry root and observes no workload marker.
fn a_refused_registration_never_starts_the_workload() {
    let scratch = Scratch::new("norn-process-registration-refusal");
    let state = scratch.join("state-is-a-file");
    let started = scratch.join("workload-started");
    fs::write(&state, b"not a directory").expect("the unusable registry root");

    let output = Command::new(env!("CARGO_BIN_EXE_norn-process"))
        .args([
            "supervise",
            "--purpose",
            "registration-refusal-test",
            "--deadline-seconds",
            "30",
            "--",
            "/bin/sh",
            "-c",
            &format!("touch '{}'", started.display()),
        ])
        .env("NORN_TEST_ISOLATION_DIR", &state)
        .output()
        .expect("running norn-process");

    assert!(!output.status.success());
    assert!(
        !started.exists(),
        "the workload ran before its registration succeeded"
    );
}

#[test]
#[allow(clippy::disallowed_methods)] // The integration seam observes registry removal after a failed workload.
fn the_supervisor_preserves_a_workload_exit_code_and_removes_its_record() {
    let scratch = Scratch::new("norn-process-exit-code");
    let state = scratch.join("state");
    let status = Command::new(env!("CARGO_BIN_EXE_norn-process"))
        .args([
            "supervise",
            "--purpose",
            "exit-code-test",
            "--deadline-seconds",
            "30",
            "--",
            "/bin/sh",
            "-c",
            "exit 7",
        ])
        .env("NORN_TEST_ISOLATION_DIR", &state)
        .status()
        .expect("running norn-process");

    assert_eq!(status.code(), Some(7));
    assert_registry_empty(&state);
}

#[test]
#[allow(clippy::disallowed_methods)] // The integration seam observes the workload's exact terminating signal.
fn the_supervisor_preserves_a_workload_signal_and_removes_its_record() {
    let scratch = Scratch::new("norn-process-workload-signal");
    let state = scratch.join("state");
    let status = Command::new(env!("CARGO_BIN_EXE_norn-process"))
        .args([
            "supervise",
            "--purpose",
            "workload-signal-test",
            "--deadline-seconds",
            "30",
            "--",
            "/bin/sh",
            "-c",
            "kill -TERM $$",
        ])
        .env("NORN_TEST_ISOLATION_DIR", &state)
        .status()
        .expect("running norn-process");

    assert_eq!(status.signal(), Some(libc::SIGTERM));
    assert_registry_empty(&state);
}

#[test]
#[allow(clippy::disallowed_methods)] // The integration seam observes clap's boundary validation.
fn the_cli_refuses_an_empty_purpose_and_an_empty_workload() {
    for arguments in [
        vec![
            "supervise",
            "--purpose",
            " ",
            "--deadline-seconds",
            "30",
            "--",
            "/bin/true",
        ],
        vec![
            "supervise",
            "--purpose",
            "missing-workload-test",
            "--deadline-seconds",
            "30",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_norn-process"))
            .args(arguments)
            .output()
            .expect("running norn-process");
        assert_eq!(output.status.code(), Some(2));
    }
}

#[test]
#[allow(clippy::disallowed_methods)] // The integration seam observes descendants after both workload outcomes.
fn successful_and_failed_workloads_leave_no_group_member() {
    for code in [0, 7] {
        let scratch = Scratch::new(&format!("norn-process-outcome-{code}"));
        let state = scratch.join("state");
        let output = Command::new(env!("CARGO_BIN_EXE_norn-process"))
            .args([
                "supervise",
                "--purpose",
                "outcome-cleanup-test",
                "--deadline-seconds",
                "30",
                "--",
                "/bin/sh",
                "-c",
                &format!("sleep 3600 & echo $!; exit {code}"),
            ])
            .env("NORN_TEST_ISOLATION_DIR", &state)
            .output()
            .expect("running norn-process");

        assert_eq!(output.status.code(), Some(code));
        let descendant: libc::pid_t = String::from_utf8(output.stdout)
            .expect("the descendant pid as text")
            .trim()
            .parse()
            .expect("a numeric descendant pid");
        assert_process_gone(descendant);
        assert_registry_empty(&state);
    }
}

#[test]
#[allow(clippy::disallowed_methods)] // The integration seam observes the deadline's group and registry cleanup.
fn a_deadline_cleans_the_group_and_removes_its_record() {
    let scratch = Scratch::new("norn-process-deadline");
    let state = scratch.join("state");
    let output = Command::new(env!("CARGO_BIN_EXE_norn-process"))
        .args([
            "supervise",
            "--purpose",
            "deadline-test",
            "--deadline-seconds",
            "1",
            "--",
            "/bin/sh",
            "-c",
            "sleep 3600 & echo $!; wait",
        ])
        .env("NORN_TEST_ISOLATION_DIR", &state)
        .output()
        .expect("running norn-process");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("reached its deadline"));
    let descendant: libc::pid_t = String::from_utf8(output.stdout)
        .expect("the descendant pid as text")
        .trim()
        .parse()
        .expect("a numeric descendant pid");
    assert_process_gone(descendant);
    assert_registry_empty(&state);
}

#[test]
#[allow(clippy::disallowed_methods)] // The integration seam signals the supervisor and observes group and registry cleanup.
fn sigint_and_sigterm_clean_the_group_before_the_supervisor_preserves_the_signal() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let scratch = Scratch::new(&format!("norn-process-signal-{signal}"));
        let state = scratch.join("state");
        let descendant_file = scratch.join("descendant-pid");
        let ready = scratch.join("ready");
        let script = format!(
            "sleep 3600 & echo $! > '{}' && touch '{}' && wait",
            descendant_file.display(),
            ready.display()
        );
        let mut supervisor = Command::new(env!("CARGO_BIN_EXE_norn-process"))
            .args([
                "supervise",
                "--purpose",
                "termination-signal-test",
                "--deadline-seconds",
                "30",
                "--",
                "/bin/sh",
                "-c",
                &script,
            ])
            .env("NORN_TEST_ISOLATION_DIR", &state)
            .spawn()
            .expect("spawning norn-process");

        wait_for_file(&ready);
        let descendant: libc::pid_t = fs::read_to_string(&descendant_file)
            .expect("the descendant pid")
            .trim()
            .parse()
            .expect("a numeric descendant pid");
        assert_registry_is_private_and_has_one_record(&state);
        assert_eq!(
            unsafe { libc::kill(supervisor.id() as libc::pid_t, signal) },
            0
        );
        let status = supervisor.wait().expect("waiting for norn-process");

        assert_eq!(status.signal(), Some(signal));
        assert_process_gone(descendant);
        assert_registry_empty(&state);
    }
}

#[test]
#[allow(clippy::disallowed_methods)] // The integration seam observes recovery state after abrupt supervisor loss.
fn abrupt_supervisor_loss_leaves_the_registered_group_for_recovery() {
    let scratch = Scratch::new("norn-process-abrupt-loss");
    let state = scratch.join("state");
    let ready = scratch.join("ready");
    let mut supervisor = Command::new(env!("CARGO_BIN_EXE_norn-process"))
        .args([
            "supervise",
            "--purpose",
            "abrupt-loss-test",
            "--deadline-seconds",
            "30",
            "--",
            "/bin/sh",
            "-c",
            &format!("touch '{}' && exec sleep 3600", ready.display()),
        ])
        .env("NORN_TEST_ISOLATION_DIR", &state)
        .spawn()
        .expect("spawning norn-process");

    wait_for_file(&ready);
    let record_path = one_registry_record(&state);
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(&record_path).expect("the live registry record"))
            .expect("a JSON registry record");
    let pgid = record["process_group"]["pid"].as_i64().unwrap() as libc::pid_t;

    assert_eq!(
        unsafe { libc::kill(supervisor.id() as libc::pid_t, libc::SIGKILL) },
        0
    );
    let status = supervisor.wait().expect("waiting for norn-process");
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert_eq!(unsafe { libc::killpg(pgid, 0) }, 0);
    assert!(
        record_path.exists(),
        "abrupt loss removed the recovery record"
    );

    // The registered leader is alive at this signal. It pins the process-group
    // identity until this controlled fixture receives its one destructive act.
    assert_eq!(unsafe { libc::kill(pgid, 0) }, 0);
    assert_eq!(unsafe { libc::killpg(pgid, libc::SIGKILL) }, 0);
    wait_for_group_absence(pgid);
    fs::remove_file(record_path).expect("removing the controlled fixture record");
    assert_registry_empty(&state);
}

#[test]
#[allow(clippy::disallowed_methods)] // The integration seam exits the workload only after abrupt supervisor loss.
fn the_launcher_stays_registered_when_the_workload_exits_after_supervisor_loss() {
    let scratch = Scratch::new("norn-process-loss-before-exit");
    let state = scratch.join("state");
    let ready = scratch.join("ready");
    let release = scratch.join("release");
    let workload_pid = scratch.join("workload-pid");
    let script = format!(
        "echo $$ > '{}' && touch '{}' && while [ ! -e '{}' ]; do sleep 0.01; done",
        workload_pid.display(),
        ready.display(),
        release.display()
    );
    let mut supervisor = Command::new(env!("CARGO_BIN_EXE_norn-process"))
        .args([
            "supervise",
            "--purpose",
            "loss-before-exit-test",
            "--deadline-seconds",
            "30",
            "--",
            "/bin/sh",
            "-c",
            &script,
        ])
        .env("NORN_TEST_ISOLATION_DIR", &state)
        .spawn()
        .expect("spawning norn-process");

    wait_for_file(&ready);
    let record_path = one_registry_record(&state);
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(&record_path).expect("the live registry record"))
            .expect("a JSON registry record");
    let pgid = record["process_group"]["pid"].as_i64().unwrap() as libc::pid_t;
    let workload: libc::pid_t = fs::read_to_string(&workload_pid)
        .expect("the workload pid")
        .trim()
        .parse()
        .expect("a numeric workload pid");

    assert_eq!(
        unsafe { libc::kill(supervisor.id() as libc::pid_t, libc::SIGKILL) },
        0
    );
    assert_eq!(
        supervisor
            .wait()
            .expect("waiting for norn-process")
            .signal(),
        Some(libc::SIGKILL)
    );
    fs::write(&release, b"release").expect("releasing the workload");
    wait_for_process_absence(workload);

    assert_eq!(unsafe { libc::kill(pgid, 0) }, 0);
    assert_eq!(unsafe { libc::killpg(pgid, 0) }, 0);
    assert!(record_path.exists());

    assert_eq!(unsafe { libc::killpg(pgid, libc::SIGKILL) }, 0);
    wait_for_group_absence(pgid);
    fs::remove_file(record_path).expect("removing the controlled fixture record");
    assert_registry_empty(&state);
}

#[test]
#[allow(clippy::disallowed_methods)] // The integration seam observes independent records from concurrent supervisors.
fn concurrent_supervisors_publish_independent_records() {
    let scratch = Scratch::new("norn-process-concurrent");
    let state = scratch.join("state");
    let release = scratch.join("release");
    let mut supervisors = Vec::new();
    for serial in 0..2 {
        let ready = scratch.join(format!("ready-{serial}"));
        let script = format!(
            "touch '{}' && while [ ! -e '{}' ]; do sleep 0.01; done",
            ready.display(),
            release.display()
        );
        let supervisor = Command::new(env!("CARGO_BIN_EXE_norn-process"))
            .args([
                "supervise",
                "--purpose",
                "concurrent-test",
                "--deadline-seconds",
                "30",
                "--",
                "/bin/sh",
                "-c",
                &script,
            ])
            .env("NORN_TEST_ISOLATION_DIR", &state)
            .spawn()
            .expect("spawning norn-process");
        supervisors.push((supervisor, ready));
    }

    for (_, ready) in &supervisors {
        wait_for_file(ready);
    }
    assert_eq!(registry_entries(&state).len(), 2);
    fs::write(&release, b"release").expect("the workload release marker");
    for (mut supervisor, _) in supervisors {
        assert!(
            supervisor
                .wait()
                .expect("waiting for norn-process")
                .success()
        );
    }
    assert_registry_empty(&state);
}

#[allow(clippy::disallowed_methods)] // The integration seam waits for a marker written by the workload.
fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} did not appear before the test deadline",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn assert_process_gone(pid: libc::pid_t) {
    let result = unsafe { libc::kill(pid, 0) };
    assert_eq!(result, -1, "process {pid} still exists");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

fn wait_for_process_absence(pid: libc::pid_t) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if unsafe { libc::kill(pid, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "process {pid} remained after its expected exit"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[allow(clippy::disallowed_methods)] // The integration seam observes the registry after controlled cleanup.
fn assert_registry_empty(state: &Path) {
    let entries = registry_entries(state);
    assert!(entries.is_empty(), "controlled cleanup left {entries:?}");
}

#[allow(clippy::disallowed_methods)] // The integration seam reads the one live recovery record.
fn one_registry_record(state: &Path) -> std::path::PathBuf {
    let entries = registry_entries(state);
    assert_eq!(entries.len(), 1);
    entries[0].path()
}

#[allow(clippy::disallowed_methods)] // The integration seam reads the process registry.
fn registry_entries(state: &Path) -> Vec<fs::DirEntry> {
    fs::read_dir(state.join("process-groups"))
        .expect("the process registry")
        .collect::<Result<Vec<_>, _>>()
        .expect("the process registry entries")
}

fn wait_for_group_absence(pgid: libc::pid_t) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if unsafe { libc::killpg(pgid, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "process group {pgid} remained after fixture cleanup"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[allow(clippy::disallowed_methods)] // The integration seam observes permissions on cleanup-authorizing state.
fn assert_registry_is_private_and_has_one_record(state: &Path) {
    let registry = state.join("process-groups");
    let directory_mode = fs::metadata(&registry)
        .expect("the process registry metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(directory_mode, 0o700);
    let entries = registry_entries(state);
    assert_eq!(entries.len(), 1);
    let record_mode = entries[0]
        .metadata()
        .expect("the registry record metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(record_mode, 0o600);
}
