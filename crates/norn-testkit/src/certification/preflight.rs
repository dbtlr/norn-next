//! The host-health preflight: whether this machine is an evidence source.
//!
//! # What it is for
//!
//! The suites carry authored work bounds — fifteen seconds for a host to reach
//! a settled state, a quarter-second poll interval waited out several times
//! over — and every one of them is a bound on *work*, sized for a machine that
//! has a core free for the work it is given. A machine that does not is a
//! machine where those bounds measure the queue rather than the product: a run
//! that ends at the bound leaves the case's own claim unproven, and a reader
//! cannot tell that from a claim the product failed.
//!
//! The answer is not a wider bound. A widened bound proves less on every
//! machine, and a load-scaled one proves something different on each. So the
//! bound stands and **the evidence is gated instead**: a degraded host is a
//! non-qualifying evidence source, and a run taken on one says nothing about
//! the candidate either way.
//!
//! This is the probe that says which kind of host it ran on. Its verdict fills
//! the qualification record's preflight slot
//! ([`super::ledger::Preflight`]), which a qualifying record must carry as
//! `admitted` — so a campaign cannot count a run nobody checked the machine
//! for. Run locally it answers the same question for a local suite failure:
//! `refused` classifies the failure as noise by ruling rather than by argument.
//!
//! # What it measures
//!
//! Two things, and the second only on Darwin.
//!
//! - **Load per core.** The one-minute load average over the machine's core
//!   count. It is the direct reading of the condition the bounds are sized
//!   against — how many runnable things are already competing for the cores the
//!   suites are about to want — and it is a whole-machine reading rather than a
//!   process one, which is what catches the hazard the suites actually meet:
//!   another checkout's `cargo test` on the same workstation.
//! - **fseventsd health, on Darwin.** The real-watcher cases subscribe to a
//!   service the machine runs once for everything on it. A saturated or bloated
//!   fseventsd answers a crowd by delivering late or by going silent, and a
//!   watcher case waiting for a delivery it will get in forty seconds fails at
//!   its bound with nothing wrong in the watcher. So the daemon's own processor
//!   share and resident set are read, and a host whose fseventsd is outside
//!   either bound is refused for the same reason a loaded one is.
//!
//! # What it refuses
//!
//! [`Refusal`] is closed, and a reading it cannot take refuses rather than
//! passing: a machine nobody could measure is exactly as unqualified as a
//! machine that measured badly, because the record's claim is *this host was
//! checked and admitted*.
//!
//! # Where the measuring happens
//!
//! [`Reading::from_host`] takes the reading itself, which is what a local run
//! wants. A lane's reading is taken before the lane builds anything —
//! [`READINGS_SCRIPT`] writes it into the job environment at [`READING`], and
//! [`Reading::observed`] prefers it — because a load average read after a cold
//! cargo build is a reading of this run's own compile rather than of the
//! machine it landed on.

use std::fmt::Write as _;

use super::ledger;

/// The environment variable a lane's early reading is carried in, as the
/// `key=value` line [`Reading::render`] writes and [`Reading::parse`] reads.
pub const READING: &str = "NORN_PREFLIGHT_READING";

/// Where the classifier writes the verdict, as `key=value` lines a lane appends
/// to the job environment. A run that names no sink writes none.
pub const SINK: &str = "NORN_PREFLIGHT_SINK";

/// The script that takes a lane's reading before the lane builds anything.
pub const READINGS_SCRIPT: &str = ".github/scripts/host-readings.sh";

/// What a lane runs to turn a reading into the verdict its record carries.
pub const CLASSIFIER: &str = "cargo test --locked -p norn-testkit --lib certification::preflight";

/// **How many runnable things per core still leave the work bounds meaning what
/// they were authored to mean.**
///
/// One. At a load average equal to the core count every runnable thing has a
/// core, and the suites' work is one more thing wanting one; past it the
/// machine is handing out time slices and a fifteen-second work bound is
/// fifteen seconds of waiting plus whatever work fits. The bound is on the
/// average rather than on an instant because a sampled instant on a machine
/// running a test suite is noise in both directions.
pub const LOAD_PER_CORE_BOUND: f64 = 1.0;

/// **The processor share a healthy fseventsd sits under.**
///
/// The daemon's steady state is near-idle: it wakes for filesystem activity,
/// numbers the events and sleeps. A quarter of a core sustained is not that,
/// and the degraded state this bound exists for was three times higher again.
pub const FSEVENTSD_CPU_BOUND: f64 = 25.0;

/// **The resident set a healthy fseventsd sits under**, in kibibytes.
///
/// Half a gibibyte. A healthy daemon holds tens of mebibytes; the degraded
/// state this bound exists for held four times this much, alongside delivery
/// latencies of five to forty seconds.
pub const FSEVENTSD_RESIDENT_BOUND_KIB: u64 = 512 * 1024;

/// What the machine said about itself.
///
/// Every field is optional in the same way and for the same reason: a reading
/// that could not be taken is a fact about the run, and [`Reading::verdict`]
/// refuses on it rather than assuming the healthy value.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Reading {
    /// The machine's core count.
    pub cores: Option<usize>,
    /// The one-minute load average, in thousandths so that a reading is a value
    /// two reads agree on and a record is comparable years later.
    pub load_milli: Option<u64>,
    /// What Darwin's filesystem-event daemon is doing.
    pub fseventsd: Fseventsd,
}

/// The Darwin filesystem-event daemon, as the process table shows it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Fseventsd {
    /// Not a Darwin host. Nothing here decides anything: the platform's real
    /// watcher is not this daemon.
    #[default]
    NotApplicable,
    /// A Darwin host whose process table could not be read. A Darwin run whose
    /// daemon nobody looked at is a run whose watcher evidence nobody can
    /// classify.
    Unread,
    /// A Darwin host with no fseventsd running. Every real-watcher case on it
    /// is waiting for a service that is not there.
    Absent,
    Running {
        /// Processor share, in tenths of a percent.
        cpu_deci_percent: u64,
        /// Resident set, in kibibytes.
        resident_kib: u64,
    },
}

/// Why a host is not an evidence source. Closed, so a host is never admitted by
/// a reason somebody invented for it.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Refusal {
    /// A measurement the verdict turns on could not be taken. Named rather than
    /// defaulted: the record's claim is that this host was checked.
    Unmeasured(&'static str),
    /// The machine is already busy. Carried in thousandths of a runnable thing
    /// per core.
    Loaded { load_per_core_milli: u64 },
    /// Darwin's event daemon is burning processor. Carried in tenths of a
    /// percent.
    FseventsdSaturated { cpu_deci_percent: u64 },
    /// Darwin's event daemon has grown. Carried in kibibytes.
    FseventsdBloated { resident_kib: u64 },
    /// Darwin, and no event daemon is running at all.
    FseventsdAbsent,
}

impl Refusal {
    /// One line a reader acts on, in the preflight's own vocabulary.
    pub fn render(&self) -> String {
        match self {
            Refusal::Unmeasured(what) => {
                format!("{what} could not be measured, so this host was not checked")
            }
            Refusal::Loaded {
                load_per_core_milli,
            } => format!(
                "{}.{:03} runnable per core, past {LOAD_PER_CORE_BOUND:.2}: the work bounds would \
                 measure the queue",
                load_per_core_milli / 1000,
                load_per_core_milli % 1000
            ),
            Refusal::FseventsdSaturated { cpu_deci_percent } => format!(
                "fseventsd at {}.{}% of a core, past {FSEVENTSD_CPU_BOUND:.0}%: real-watcher \
                 deliveries are late on this host",
                cpu_deci_percent / 10,
                cpu_deci_percent % 10
            ),
            Refusal::FseventsdBloated { resident_kib } => format!(
                "fseventsd resident at {resident_kib} KiB, past {FSEVENTSD_RESIDENT_BOUND_KIB} \
                 KiB: real-watcher deliveries are late on this host"
            ),
            Refusal::FseventsdAbsent => {
                "no fseventsd is running, so this host delivers no real watcher events".to_string()
            }
        }
    }
}

/// Whether this host is an evidence source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Verdict {
    /// Every measurement was taken and every one is inside its bound.
    Admitted,
    /// At least one reason, and every reason that applies. A reader fixing a
    /// host wants all of them rather than the first.
    Refused(Vec<Refusal>),
}

impl Verdict {
    /// The spelling [`ledger::PREFLIGHT`] carries, which is the ledger's
    /// vocabulary rather than this module's.
    pub fn spelling(&self) -> &'static str {
        match self {
            Verdict::Admitted => "admitted",
            Verdict::Refused(_) => "refused",
        }
    }

    pub fn admitted(&self) -> bool {
        matches!(self, Verdict::Admitted)
    }
}

impl Reading {
    /// **The verdict this reading implies**, with every reason that applies.
    ///
    /// Nothing here reads the machine: a reading is the whole input, which is
    /// what makes the classification testable over a degraded host nobody has
    /// to arrange.
    pub fn verdict(&self) -> Verdict {
        let mut refusals = Vec::new();
        match (self.cores, self.load_milli) {
            (Some(cores), Some(load_milli)) if cores > 0 => {
                let load_per_core_milli = load_milli / cores as u64;
                if load_per_core_milli > (LOAD_PER_CORE_BOUND * 1000.0) as u64 {
                    refusals.push(Refusal::Loaded {
                        load_per_core_milli,
                    });
                }
            }
            (None, _) | (Some(0), _) => refusals.push(Refusal::Unmeasured("the core count")),
            (Some(_), None) => refusals.push(Refusal::Unmeasured("the load average")),
            (Some(_), Some(_)) => {}
        }
        match self.fseventsd {
            Fseventsd::NotApplicable => {}
            Fseventsd::Unread => refusals.push(Refusal::Unmeasured("fseventsd")),
            Fseventsd::Absent => refusals.push(Refusal::FseventsdAbsent),
            Fseventsd::Running {
                cpu_deci_percent,
                resident_kib,
            } => {
                if cpu_deci_percent > (FSEVENTSD_CPU_BOUND * 10.0) as u64 {
                    refusals.push(Refusal::FseventsdSaturated { cpu_deci_percent });
                }
                if resident_kib > FSEVENTSD_RESIDENT_BOUND_KIB {
                    refusals.push(Refusal::FseventsdBloated { resident_kib });
                }
            }
        }
        if refusals.is_empty() {
            Verdict::Admitted
        } else {
            Verdict::Refused(refusals)
        }
    }

    /// The reading as one line of `key=value` pairs — what
    /// [`READINGS_SCRIPT`] writes and [`Reading::parse`] reads back.
    ///
    /// One line, because it travels through a job environment and a record
    /// field, and both are line-oriented.
    pub fn render(&self) -> String {
        let mut rendered = String::new();
        if let Some(cores) = self.cores {
            let _ = write!(rendered, "cores={cores} ");
        }
        if let Some(load_milli) = self.load_milli {
            let _ = write!(rendered, "load-milli={load_milli} ");
        }
        match self.fseventsd {
            Fseventsd::NotApplicable => {}
            Fseventsd::Unread => rendered.push_str("fseventsd=unread "),
            Fseventsd::Absent => rendered.push_str("fseventsd=absent "),
            Fseventsd::Running {
                cpu_deci_percent,
                resident_kib,
            } => {
                let _ = write!(
                    rendered,
                    "fseventsd=running fseventsd-cpu-deci={cpu_deci_percent} \
                     fseventsd-rss-kib={resident_kib} "
                );
            }
        }
        rendered.trim_end().to_string()
    }

    /// Read back what [`Reading::render`] wrote. A pair this cannot read leaves
    /// its field unmeasured, which the verdict refuses on rather than guesses.
    pub fn parse(line: &str) -> Reading {
        let mut reading = Reading::default();
        let mut cpu_deci = None;
        let mut resident = None;
        let mut running = false;
        for pair in line.split_whitespace() {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            match key {
                "cores" => reading.cores = value.parse().ok(),
                "load-milli" => reading.load_milli = value.parse().ok(),
                "fseventsd" => match value {
                    "running" => running = true,
                    "absent" => reading.fseventsd = Fseventsd::Absent,
                    _ => reading.fseventsd = Fseventsd::Unread,
                },
                "fseventsd-cpu-deci" => cpu_deci = value.parse().ok(),
                "fseventsd-rss-kib" => resident = value.parse().ok(),
                _ => {}
            }
        }
        if running {
            reading.fseventsd = match (cpu_deci, resident) {
                (Some(cpu_deci_percent), Some(resident_kib)) => Fseventsd::Running {
                    cpu_deci_percent,
                    resident_kib,
                },
                _ => Fseventsd::Unread,
            };
        }
        reading
    }

    /// **The reading this run is classified on**: the one a lane took before it
    /// built anything, where the environment carries one, and otherwise the
    /// machine's own answer right now.
    ///
    /// The preference is the whole point of the variable. A lane's cold build
    /// is minutes of every core, so a load average read after it is a reading
    /// of this run's compile rather than of the host it landed on.
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: reads the reading a lane took before it built.
    pub fn observed() -> Reading {
        match std::env::var(READING) {
            Ok(line) if !line.trim().is_empty() => Reading::parse(&line),
            _ => Reading::from_host(),
        }
    }

    /// Ask this machine, now.
    pub fn from_host() -> Reading {
        Reading {
            cores: std::thread::available_parallelism().ok().map(Into::into),
            load_milli: load_average_milli(),
            fseventsd: fseventsd(),
        }
    }
}

/// **The record the preflight leaves**, as the `key=value` lines a lane appends
/// to its job environment and the ledger then reads.
///
/// The keys are the ledger's, not this module's: what the record carries is a
/// verdict and a detail string, and the ledger's own documentation is explicit
/// that it records the verdict rather than reaching one. This is the writer on
/// the other side of that split.
pub fn rendered_verdict(reading: &Reading, verdict: &Verdict) -> String {
    format!(
        "{}={}\n{}={}\n",
        ledger::PREFLIGHT,
        verdict.spelling(),
        ledger::PREFLIGHT_DETAIL,
        detail(reading, verdict)
    )
}

/// One line naming what was measured and, where the host was refused, every
/// reason it was. This is what the record's `detail` slot carries.
pub fn detail(reading: &Reading, verdict: &Verdict) -> String {
    let measured = reading.render();
    let measured = if measured.is_empty() {
        "nothing measured".to_string()
    } else {
        measured
    };
    match verdict {
        Verdict::Admitted => format!("admitted: {measured}"),
        Verdict::Refused(refusals) => {
            let reasons: Vec<String> = refusals.iter().map(Refusal::render).collect();
            format!("refused: {measured} — {}", reasons.join("; "))
        }
    }
}

/// The one-minute load average in thousandths.
///
/// Read from the place each platform keeps it rather than through a crate: the
/// two spellings are three lines each, and a dependency added to the harness is
/// a dependency in the graph the architecture gate holds.
#[cfg(target_os = "linux")]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: this host's own load average.
fn load_average_milli() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/loadavg").ok()?;
    milli(text.split_whitespace().next()?)
}

#[cfg(target_os = "macos")]
fn load_average_milli() -> Option<u64> {
    // `{ 1.23 4.56 7.89 }`, oldest last.
    let reported = command("sysctl", &["-n", "vm.loadavg"])?;
    milli(
        reported
            .trim()
            .trim_start_matches('{')
            .trim()
            .split_whitespace()
            .next()?,
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn load_average_milli() -> Option<u64> {
    None
}

/// A decimal reading in thousandths, so a reading is an integer a record can be
/// compared on.
fn milli(reported: &str) -> Option<u64> {
    let (whole, fraction) = reported.split_once('.').unwrap_or((reported, "0"));
    let fraction: String = fraction.chars().take(3).collect();
    let scale = 10u64.pow(3 - fraction.len() as u32);
    Some(whole.parse::<u64>().ok()? * 1000 + fraction.parse::<u64>().ok()? * scale)
}

#[cfg(target_os = "macos")]
fn fseventsd() -> Fseventsd {
    let Some(table) = command("ps", &["-Axo", "%cpu=,rss=,comm="]) else {
        return Fseventsd::Unread;
    };
    for line in table.lines() {
        let mut fields = line.split_whitespace();
        let (Some(cpu), Some(resident), Some(command)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if std::path::Path::new(command)
            .file_name()
            .and_then(|name| name.to_str())
            != Some("fseventsd")
        {
            continue;
        }
        let (Some(cpu_deci_percent), Ok(resident_kib)) =
            (milli(cpu).map(|milli| milli / 100), resident.parse())
        else {
            return Fseventsd::Unread;
        };
        return Fseventsd::Running {
            cpu_deci_percent,
            resident_kib,
        };
    }
    Fseventsd::Absent
}

#[cfg(not(target_os = "macos"))]
fn fseventsd() -> Fseventsd {
    Fseventsd::NotApplicable
}

/// A platform reading taken through the tool that owns it, with a tool that is
/// missing or angry reading as no answer at all.
#[cfg(target_os = "macos")]
#[allow(clippy::disallowed_methods)] // Harness scaffolding: this host's own process table.
fn command(program: &str, arguments: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(arguments)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host with nothing wrong with it: eight cores, a load of two, and on
    /// Darwin a daemon doing nothing.
    fn healthy() -> Reading {
        Reading {
            cores: Some(8),
            load_milli: Some(2_000),
            fseventsd: Fseventsd::Running {
                cpu_deci_percent: 3,
                resident_kib: 40 * 1024,
            },
        }
    }

    /// **A quiet host is admitted.** Every measurement taken, every one inside
    /// its bound.
    #[test]
    fn a_quiet_host_is_admitted() {
        assert_eq!(healthy().verdict(), Verdict::Admitted);
    }

    /// **A host with no Darwin daemon to read is admitted on its load alone.**
    /// The Linux lane's real watcher is inotify, and a bound on a service it
    /// does not run would refuse every Linux run there is.
    #[test]
    fn a_host_that_is_not_darwin_is_judged_on_load_alone() {
        assert_eq!(
            Reading {
                fseventsd: Fseventsd::NotApplicable,
                ..healthy()
            }
            .verdict(),
            Verdict::Admitted
        );
    }

    /// **Exactly one runnable thing per core is still admitted**, and the first
    /// thousandth past it is not. The bound is where the machine stops having a
    /// core for everything that wants one.
    #[test]
    fn the_load_bound_admits_a_full_machine_and_refuses_an_oversubscribed_one() {
        assert_eq!(
            Reading {
                load_milli: Some(8_000),
                ..healthy()
            }
            .verdict(),
            Verdict::Admitted
        );
        assert_eq!(
            Reading {
                load_milli: Some(8_008),
                ..healthy()
            }
            .verdict(),
            Verdict::Refused(vec![Refusal::Loaded {
                load_per_core_milli: 1_001
            }])
        );
    }

    /// **The workstation the ledger was collected on is refused.** Twelve
    /// cores under a load of ten is the class-A starvation condition, and a
    /// run taken there is not evidence about the candidate.
    #[test]
    fn an_oversubscribed_workstation_is_refused() {
        let verdict = Reading {
            cores: Some(12),
            load_milli: Some(21_600),
            ..healthy()
        }
        .verdict();
        assert_eq!(
            verdict,
            Verdict::Refused(vec![Refusal::Loaded {
                load_per_core_milli: 1_800
            }])
        );
        assert_eq!(verdict.spelling(), "refused");
    }

    /// **The degraded daemon this bound exists for is refused, on both
    /// readings and at once.** Every reason that applies is carried: a reader
    /// fixing the host needs all of them, and a verdict that stopped at the
    /// first would send them back for the second.
    #[test]
    fn a_degraded_event_daemon_is_refused_on_every_reading_that_applies() {
        assert_eq!(
            Reading {
                fseventsd: Fseventsd::Running {
                    cpu_deci_percent: 760,
                    resident_kib: 2_200_000,
                },
                ..healthy()
            }
            .verdict(),
            Verdict::Refused(vec![
                Refusal::FseventsdSaturated {
                    cpu_deci_percent: 760
                },
                Refusal::FseventsdBloated {
                    resident_kib: 2_200_000
                },
            ])
        );
    }

    /// **A Darwin host with no daemon, and one whose table would not read, are
    /// both refused.** A real-watcher case on either is waiting on something
    /// nobody can account for.
    #[test]
    fn a_darwin_host_without_a_readable_daemon_is_refused() {
        assert_eq!(
            Reading {
                fseventsd: Fseventsd::Absent,
                ..healthy()
            }
            .verdict(),
            Verdict::Refused(vec![Refusal::FseventsdAbsent])
        );
        assert_eq!(
            Reading {
                fseventsd: Fseventsd::Unread,
                ..healthy()
            }
            .verdict(),
            Verdict::Refused(vec![Refusal::Unmeasured("fseventsd")])
        );
    }

    /// **A measurement nobody took refuses.** The record's claim is that this
    /// host was checked, so an unread load average or core count is exactly as
    /// unqualified as a bad one — and never quietly the healthy value.
    #[test]
    fn a_reading_that_was_not_taken_refuses_rather_than_assuming_health() {
        for unmeasured in [
            Reading {
                cores: None,
                ..healthy()
            },
            Reading {
                cores: Some(0),
                ..healthy()
            },
            Reading {
                load_milli: None,
                ..healthy()
            },
            Reading::default(),
        ] {
            assert!(
                !unmeasured.verdict().admitted(),
                "{unmeasured:?} was admitted with a measurement missing"
            );
        }
    }

    /// A reading survives the job environment it travels through: rendered to
    /// one line and read back, it is the same reading and therefore the same
    /// verdict.
    #[test]
    fn a_reading_round_trips_through_the_line_a_lane_carries_it_on() {
        for reading in [
            healthy(),
            Reading {
                fseventsd: Fseventsd::NotApplicable,
                ..healthy()
            },
            Reading {
                fseventsd: Fseventsd::Absent,
                ..healthy()
            },
            Reading {
                fseventsd: Fseventsd::Unread,
                ..healthy()
            },
            Reading::default(),
        ] {
            let rendered = reading.render();
            assert!(!rendered.contains('\n'), "{rendered:?} is not one line");
            assert_eq!(Reading::parse(&rendered), reading, "{rendered:?}");
        }
    }

    /// The detail a record carries names every refusal, on one line, because a
    /// record field and a job environment are both line-oriented.
    #[test]
    fn the_detail_names_every_refusal_on_one_line() {
        let reading = Reading {
            cores: Some(12),
            load_milli: Some(21_600),
            fseventsd: Fseventsd::Running {
                cpu_deci_percent: 760,
                resident_kib: 2_200_000,
            },
        };
        let verdict = reading.verdict();
        let detail = detail(&reading, &verdict);
        assert!(!detail.contains('\n'), "{detail:?}");
        assert!(detail.starts_with("refused: "), "{detail}");
        assert!(detail.contains("runnable per core"), "{detail}");
        assert!(detail.contains("fseventsd at 76.0%"), "{detail}");
        assert!(detail.contains("fseventsd resident at"), "{detail}");

        let rendered = rendered_verdict(&reading, &verdict);
        assert!(
            rendered.starts_with(&format!("{}=refused\n", ledger::PREFLIGHT)),
            "{rendered}"
        );
        assert!(
            rendered.contains(&format!("{}=refused: ", ledger::PREFLIGHT_DETAIL)),
            "{rendered}"
        );
        assert_eq!(rendered.lines().count(), 2, "{rendered}");
    }

    /// **The host this run is on, classified and printed.**
    ///
    /// It asserts nothing about the machine — a loaded workstation is a fact
    /// about the workstation — and it is how a local suite failure is
    /// classified: `refused` here says the run was taken on a host the ruling
    /// excludes as an evidence source, so the failure is noise rather than
    /// signal. In a lane it is also the writer: the verdict goes where
    /// [`SINK`] names, and the lane appends that file to the job environment
    /// the record is assembled from.
    #[test]
    #[allow(clippy::disallowed_methods)] // Harness scaffolding: writes the verdict where the lane named it.
    fn this_host_is_read_and_classified() {
        let reading = Reading::observed();
        let verdict = reading.verdict();
        eprintln!("host-health preflight: {}", detail(&reading, &verdict));
        if let Some(sink) = std::env::var_os(SINK) {
            let rendered = rendered_verdict(&reading, &verdict);
            std::fs::write(&sink, rendered.as_bytes()).unwrap_or_else(|problem| {
                panic!(
                    "writing the preflight verdict to {}: {problem}",
                    std::path::Path::new(&sink).display()
                )
            });
        }
    }
}
