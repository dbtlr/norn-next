//! Filling what a rule declares: the one way a rule's default or route is
//! bound to the path it is for and filled.
//!
//! The defaults fixpoint ([`VaultSchema::fill_rule_defaults`](crate::schema::VaultSchema::fill_rule_defaults)) fills a
//! created document's defaults by [`Rule::fill_default`], and repair fills a
//! missing field's default by it and a misplaced document's route by
//! [`Rule::fill_route`], so a capture is bound, the clock read and the work
//! tallied ([`RuleWork`]) one way whichever of them asks.

use std::collections::BTreeMap;
use std::fmt;

use norn_wire::{AuthoredValue, Binding, Captures, CaseFold};

use super::super::template::{FillError, LocalTimestamp, NotALocalTimestamp};
use super::{Rule, RuleWork};

/// The path bindings taken so far for one path: each rule's `match.path`
/// bound against it once, however many of the rule's defaults or routes read
/// a capture.
#[derive(Debug, Default)]
pub struct PathBindings<'s> {
    bound: BTreeMap<&'s str, Captures>,
}

/// Why a rule's default or route fills to no value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FillRefusal {
    /// It reads a capture its rule's `match.path` binds several ways in the
    /// path.
    AmbiguousCapture {
        /// Two of the bindings.
        bindings: Box<[Captures; 2]>,
    },
    /// It reads the clock, which gives no reading a template can fill.
    NoClockReading(NotALocalTimestamp),
    /// It fills to no value: a route's capture that cannot stand in a path
    /// segment ([`FillError::UnsafeValue`]). Schema read holds a default's
    /// tokens to the clock's and its own rule's captures, so a default never
    /// fills to none.
    Unfillable(FillError),
}

impl fmt::Display for FillRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FillRefusal::AmbiguousCapture { bindings } => write!(
                formatter,
                "a path capture its `match.path` binds several ways: {} and {}",
                spelled(&bindings[0]),
                spelled(&bindings[1])
            ),
            FillRefusal::NoClockReading(unread) => write!(
                formatter,
                "the clock cannot be read as a local time a template can fill: {unread}"
            ),
            FillRefusal::Unfillable(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for FillRefusal {}

/// One binding of a rule's captures, as a refusal names it.
fn spelled(captures: &Captures) -> String {
    let named: Vec<String> = captures
        .iter()
        .map(|(name, segment)| format!("{name}={segment}"))
        .collect();
    format!("{{{}}}", named.join(", "))
}

impl Rule {
    /// The default this rule declares for the required `field`, filled for a
    /// document standing at `path`; `None` where it declares none.
    ///
    /// A `{{path.<name>}}` is read from what the rule's own `match.path` binds
    /// in `path` (its letters compared as `case` says), a clock token from
    /// `clock`, which is called only for a default reading one. The clock is
    /// asked before the path is bound, so a clock giving no reading refuses
    /// the default even where its capture is bound several ways.
    ///
    /// **Its work is tallied in `work`**: a path binding taken, once for a
    /// rule per `bindings` however many of its defaults read it
    /// ([`RuleWork::captures_bound`]).
    ///
    /// Read by the defaults fixpoint ([`VaultSchema::fill_rule_defaults`](crate::schema::VaultSchema::fill_rule_defaults))
    /// and by repair's declared fix (`norn-host`'s `planner::repair::declared`),
    /// which fills one selected field's default the same way.
    pub fn fill_default<'s>(
        &'s self,
        field: &str,
        path: &str,
        case: CaseFold,
        clock: &mut dyn FnMut() -> Result<LocalTimestamp, NotALocalTimestamp>,
        bindings: &mut PathBindings<'s>,
        work: &mut RuleWork,
    ) -> Option<Result<AuthoredValue, FillRefusal>> {
        let default = self.required.get(field)?.as_ref()?;
        Some(
            self.read(
                default.reads_clock(),
                default.reads_captures(),
                path,
                case,
                clock,
                bindings,
                work,
            )
            .and_then(|(at, captures)| default.fill(at, captures).map_err(FillRefusal::Unfillable)),
        )
    }

    /// The folder this rule's `allowed_paths` route sends a document standing
    /// at `path` to, filled; `None` where the rule declares no route.
    ///
    /// Filled as [`Rule::fill_default`] fills a default — captures bound,
    /// clock read, work tallied alike — and each value judged as a creation
    /// rule's target judges one: a capture
    /// holding `:` or slugging to nothing fills no folder. The caller
    /// judges the whole destination as a document path too.
    pub fn fill_route<'s>(
        &'s self,
        path: &str,
        case: CaseFold,
        clock: &mut dyn FnMut() -> Result<LocalTimestamp, NotALocalTimestamp>,
        bindings: &mut PathBindings<'s>,
        work: &mut RuleWork,
    ) -> Option<Result<String, FillRefusal>> {
        let route = self.allowed_paths.as_ref()?.route.as_ref()?;
        Some(
            self.read(
                route.reads_clock(),
                route.reads_captures(),
                path,
                case,
                clock,
                bindings,
                work,
            )
            .and_then(|(at, captures)| route.fill(at, captures).map_err(FillRefusal::Unfillable)),
        )
    }

    /// The clock reading and the captures a fill reads, each taken only where
    /// the fill reads it: the clock first, then the rule's `match.path` bound
    /// in `path` once per `bindings`, tallied in `work`.
    #[allow(clippy::too_many_arguments)] // One fill's whole context: splitting it would only rename the arguments.
    fn read<'s>(
        &'s self,
        reads_clock: bool,
        reads_captures: bool,
        path: &str,
        case: CaseFold,
        clock: &mut dyn FnMut() -> Result<LocalTimestamp, NotALocalTimestamp>,
        bindings: &mut PathBindings<'s>,
        work: &mut RuleWork,
    ) -> Result<(Option<LocalTimestamp>, Captures), FillRefusal> {
        let at = if reads_clock {
            Some(clock().map_err(FillRefusal::NoClockReading)?)
        } else {
            None
        };
        let captures = if reads_captures {
            match bindings.bound.get(self.name.as_str()) {
                Some(captures) => captures.clone(),
                None => {
                    work.captures_bound += 1;
                    let captures = match self
                        .selector
                        .path
                        .as_ref()
                        .map(|glob| glob.bind(path, case))
                    {
                        Some(Binding::Unique(captures)) => captures,
                        Some(Binding::Several(several)) => {
                            return Err(FillRefusal::AmbiguousCapture { bindings: several });
                        }
                        // A rule that selects the path matches it; a default
                        // or route reading a capture is refused at read where
                        // the rule has no `match.path`.
                        Some(Binding::Unmatched) | None => Captures::default(),
                    };
                    bindings.bound.insert(&self.name, captures.clone());
                    captures
                }
            }
        } else {
            Captures::default()
        };
        Ok((at, captures))
    }
}
