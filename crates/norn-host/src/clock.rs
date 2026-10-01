//! The one clock reading a plan's templates are filled from.
//!
//! A creation rule's `{{now}}`, `{{date}}` and `{{time}}` are local: they
//! state the day and the time where the host runs, and the offset that zone
//! stood at. `norn-config` fills a template from a [`LocalTimestamp`] it is
//! handed and reads no clock, so the clock is read here, once per plan, and
//! the reading is what every template of that plan fills from.

use jiff::Timestamp;
use jiff::tz::{Offset, TimeZone};
use norn_config::schema::{LocalTimestamp, NotALocalTimestamp};

/// Now, in the system's time zone, or the refusal of a system clock set
/// outside the years 0 to 9999, which `{{date}}` cannot write.
///
/// Two zones read as UTC. A system zone jiff cannot resolve, jiff itself
/// reads as UTC: `TZ=EST5EDT` is one where the zone database holds no
/// `EST5EDT`, which glibc reads as Eastern time and jiff cannot read at all. A
/// zone whose offset is a day or more from UTC, which POSIX allows and
/// RFC 3339 cannot write, reads as UTC here.
///
/// **A dormant carrier.** Its consuming layer is the planner's expansion of a
/// create-by-rule operation into the document it makes (NORN-298), which
/// reads the clock once per plan and fills every template of the plan from
/// this one reading. No operation that creates by rule is planned yet, so
/// nothing in the call graph reads the clock.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "a dormant carrier: the create-by-rule planner is its consumer"
    )
)]
pub(crate) fn local_now() -> Result<LocalTimestamp, NotALocalTimestamp> {
    local_timestamp(Timestamp::now(), &TimeZone::system())
}

/// `instant` as the local reading `zone` gives it, or as UTC gives it where
/// no reading holds what `zone` gives: an offset of a day or more.
fn local_timestamp(
    instant: Timestamp,
    zone: &TimeZone,
) -> Result<LocalTimestamp, NotALocalTimestamp> {
    reading(instant, zone).or_else(|_| reading(instant, &TimeZone::UTC))
}

/// `instant` as the local reading `zone` gives it.
///
/// An offset is read to the minute, the precision `{{now}}` writes it in, and
/// the time is read at that same offset, so the reading always names
/// `instant`. Every offset a zone holds today is a whole number of minutes; an
/// offset with seconds in it (a historical one, or a POSIX zone written so) is
/// truncated toward zero, and the time moves with it.
fn reading(instant: Timestamp, zone: &TimeZone) -> Result<LocalTimestamp, NotALocalTimestamp> {
    let stated = instant.to_zoned(zone.clone()).offset().seconds() / 60;
    let offset_minutes = i16::try_from(stated).map_err(|_| NotALocalTimestamp)?;
    let at_minutes = Offset::from_seconds(stated * 60).map_err(|_| NotALocalTimestamp)?;
    let local = instant.to_zoned(TimeZone::fixed(at_minutes));
    let part = |value: i8| u8::try_from(value).map_err(|_| NotALocalTimestamp);
    LocalTimestamp::new(
        local.year().into(),
        part(local.month())?,
        part(local.day())?,
        part(local.hour())?,
        part(local.minute())?,
        part(local.second())?,
        offset_minutes,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use norn_config::schema::{Template, TemplateValues};

    use super::*;

    /// `instant` read in `zone`, as `{{now}}` fills it.
    fn now_in(instant: &str, zone: &TimeZone) -> String {
        let instant: Timestamp = instant.parse().expect("an instant");
        Template::parse("{{now}}")
            .expect("a template")
            .fill(&TemplateValues::new(
                BTreeMap::new(),
                local_timestamp(instant, zone).expect("an instant in the years 0 to 9999"),
            ))
            .expect("the template fills")
    }

    /// **One instant reads as its zone's day, time and offset**, whatever
    /// zone the machine running the case is in: the zone is named, and it
    /// moves its offset across a daylight-saving change.
    #[test]
    fn an_instant_reads_as_its_zones_day_time_and_offset() {
        let berlin = TimeZone::posix("CET-1CEST,M3.5.0,M10.5.0/3").expect("a POSIX zone");
        assert_eq!(
            now_in("2026-10-01T17:00:00Z", &berlin),
            "2026-10-01T19:00:00+02:00"
        );
        assert_eq!(
            now_in("2026-12-31T23:30:45Z", &berlin),
            "2027-01-01T00:30:45+01:00"
        );
        let brasilia = TimeZone::fixed(jiff::tz::offset(-3));
        assert_eq!(
            now_in("2026-10-01T02:15:09Z", &brasilia),
            "2026-09-30T23:15:09-03:00"
        );
        assert_eq!(
            now_in("2026-10-01T17:00:00Z", &TimeZone::UTC),
            "2026-10-01T17:00:00+00:00"
        );
    }

    /// A zone whose offset is not whole hours keeps its minutes.
    #[test]
    fn an_offset_keeps_its_minutes() {
        let india = TimeZone::posix("IST-5:30").expect("a POSIX zone");
        assert_eq!(
            now_in("2026-10-01T17:00:00Z", &india),
            "2026-10-01T22:30:00+05:30"
        );
    }

    /// **An offset with seconds in it is read at its minutes, and so is the
    /// time**: the reading names the instant it was taken at, whatever
    /// precision the zone states its offset in.
    #[test]
    fn an_offset_with_seconds_reads_the_time_at_its_minutes() {
        let zone = TimeZone::posix("XXX-5:30:45").expect("a POSIX zone");
        assert_eq!(
            now_in("2026-10-01T17:00:00Z", &zone),
            "2026-10-01T22:30:00+05:30"
        );
        let west = TimeZone::posix("XXX23:59:59").expect("a POSIX zone");
        assert_eq!(
            now_in("2026-10-01T17:00:00Z", &west),
            "2026-09-30T17:01:00-23:59"
        );
    }

    /// **A zone whose offset is a day or more reads as UTC.** POSIX allows
    /// such an offset and a reading cannot write one, since RFC 3339 writes an
    /// offset's hours as 00 to 23; the reading falls back to UTC, as a zone
    /// that cannot be resolved does, rather than refusing the clock.
    #[test]
    fn a_zone_a_day_or_more_from_utc_reads_as_utc() {
        for posix in ["<+24>-24", "<-2430>24:30"] {
            let zone = TimeZone::posix(posix).expect("a POSIX zone");
            assert_eq!(
                now_in("2026-10-01T17:00:00Z", &zone),
                "2026-10-01T17:00:00+00:00",
                "{posix}"
            );
        }
    }

    /// **The dormant carrier is covered at its seam**: the system clock, in
    /// the system's zone, fills `{{now}}` with the instant it is, so the
    /// offset it states has the sign that turns the local time back into
    /// that instant.
    #[test]
    fn the_system_clock_reads_as_a_local_timestamp() {
        let before = Timestamp::now();
        let reading = local_now().expect("the system clock reads in the years 0 to 9999");
        let now = Template::parse("{{now}}")
            .expect("a template")
            .fill(&TemplateValues::new(BTreeMap::new(), reading))
            .expect("the template fills");
        let read_back: Timestamp = now.parse().expect("`{{now}}` is RFC 3339");
        let drift = read_back.duration_since(before).abs();
        assert!(
            drift <= jiff::SignedDuration::from_secs(5),
            "`{now}` is {drift:?} from the instant it was read at"
        );
    }
}
