//! Typed frontmatter values, and the one comparison rule every reader uses.
//!
//! A frontmatter value is written as text. What ordering that text has is the
//! vault schema's answer: a field declared a date orders chronologically, a
//! field declared a number orders numerically, and a field nothing declares
//! orders as the text it is. This module is where that answer is applied, so
//! that a sort, a range predicate and a comparison operator cannot come to hold
//! three rules.
//!
//! # The typed value is a projection, and it carries a sort key
//!
//! [`TypedValue`] is totally ordered, and [`TypedValue::sort_key`] is a text
//! encoding whose bytewise order is that same order. Derivation writes the key
//! beside the raw value so that a typed sort is a seek along an index rather
//! than a per-row re-parse, and a reader that already holds the value compares
//! it directly. The two agree by construction, and the suite holds them equal
//! over a mixed sample.
//!
//! # The mixed-offset signal
//!
//! A date is written three ways: a calendar day (`2026-03-04`), an instant with
//! an offset (`2026-03-04T09:00:00Z`, `...+02:00`), and a wall-clock reading
//! with no offset at all (`2026-03-04T09:00:00`). The first and third name a
//! time that depends on where the reader is standing; the second names one
//! instant. Comparing an instant against a wall-clock reading is therefore a
//! comparison whose answer depends on a zone nobody stated.
//!
//! The comparison still answers — refusing would make a field unsortable
//! because one document in the vault wrote its dates the other way — and it
//! **signals**, through [`Comparison::signal`]. The rule is exact: the signal
//! is raised when one side carries an offset and the other does not, and never
//! when both do or neither does. Two different fixed offsets are two instants
//! and compare without ambiguity.

use std::cmp::Ordering;
use std::fmt;

/// The declared type of a frontmatter field.
///
/// Plain rather than extensible: a reader that dispatches on the declared type
/// of a field has to have an answer for every type the schema can declare, and
/// a new member should break that match rather than fall into a default arm
/// that compares the new type as text.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum FieldType {
    /// Any string. This is what an undeclared field is read as too, which is
    /// why text is the floor rather than a refusal.
    Text,
    /// A number, ordered numerically.
    Number,
    /// `true` or `false`.
    Boolean,
    /// A calendar day or an instant, ordered chronologically.
    Date,
    /// A set of tag names. The facet decides which names are declared; the type
    /// decides that the field's elements are tags rather than free text.
    Tags,
}

impl FieldType {
    /// Every type the vocabulary holds, in declaration order.
    pub const ALL: [FieldType; 5] = [
        FieldType::Text,
        FieldType::Number,
        FieldType::Boolean,
        FieldType::Date,
        FieldType::Tags,
    ];

    /// The type as the schema spells it.
    pub const fn as_str(self) -> &'static str {
        match self {
            FieldType::Text => "text",
            FieldType::Number => "number",
            FieldType::Boolean => "boolean",
            FieldType::Date => "date",
            FieldType::Tags => "tags",
        }
    }

    /// The type a schema's spelling names, or nothing.
    pub fn named(spelling: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == spelling)
    }

    /// Reads `raw` as this type, or says it is not one.
    ///
    /// The only total arm is [`FieldType::Text`]: every string is text. A
    /// document whose field does not read as its declared type is a fact about
    /// that document, which is why the refusal is a value a caller decides
    /// about rather than an error that stops a derivation.
    pub fn read(self, raw: &str) -> Result<TypedValue, NotThisType> {
        match self {
            FieldType::Text | FieldType::Tags => Ok(TypedValue::Text(raw.to_string())),
            FieldType::Number => raw
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite())
                .map(TypedValue::Number)
                .ok_or(NotThisType { declared: self }),
            FieldType::Boolean => match raw.trim() {
                "true" => Ok(TypedValue::Boolean(true)),
                "false" => Ok(TypedValue::Boolean(false)),
                _ => Err(NotThisType { declared: self }),
            },
            FieldType::Date => read_date(raw.trim())
                .map(TypedValue::Date)
                .ok_or(NotThisType { declared: self }),
        }
    }
}

impl fmt::Display for FieldType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A raw value that does not read as the type its field declares.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NotThisType {
    /// The type the field declares, which the value is not.
    pub declared: FieldType,
}

impl fmt::Display for NotThisType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "the value is not {}", self.declared)
    }
}

impl std::error::Error for NotThisType {}

/// Whether a date named an instant, and which one.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Offset {
    /// A calendar day or a wall-clock reading. It names an instant only once a
    /// reader supplies a zone, and no reader here does.
    Unstated,
    /// An explicit offset from UTC, in minutes. `Z` is zero.
    Stated(i32),
}

/// A date or instant, held as the UTC second it names when its offset is
/// applied, and as the offset that was applied.
///
/// A calendar day is the second its midnight would be at offset zero, so a
/// day sorts before every reading inside it and after every reading before it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DateValue {
    seconds: i64,
    offset: Offset,
}

impl DateValue {
    /// The UTC second this value names, reading an unstated offset as zero.
    pub fn seconds(self) -> i64 {
        self.seconds
    }

    /// Whether the written form stated an offset.
    pub fn offset(self) -> Offset {
        self.offset
    }
}

/// One frontmatter value, read under its field's declared type.
///
/// The order is total and is the order every typed read uses. Across variants
/// it is the declaration order below, which matters only for a field whose
/// declaration moved under rows derived before the move: within one declared
/// type every value is one variant.
#[derive(Clone, Debug, PartialEq)]
pub enum TypedValue {
    Text(String),
    Number(f64),
    Boolean(bool),
    Date(DateValue),
}

impl Eq for TypedValue {}

impl Ord for TypedValue {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (TypedValue::Text(left), TypedValue::Text(right)) => left.cmp(right),
            // `total_cmp` rather than `partial_cmp`: the order has to be total
            // for a sort to be a seek, and the parse already refuses the
            // non-finite values that make it partial.
            (TypedValue::Number(left), TypedValue::Number(right)) => left.total_cmp(right),
            (TypedValue::Boolean(left), TypedValue::Boolean(right)) => left.cmp(right),
            (TypedValue::Date(left), TypedValue::Date(right)) => left.seconds.cmp(&right.seconds),
            (left, right) => left.rank().cmp(&right.rank()),
        }
    }
}

impl PartialOrd for TypedValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl TypedValue {
    /// The variant's position in the cross-variant order.
    const fn rank(&self) -> u8 {
        match self {
            TypedValue::Text(_) => 0,
            TypedValue::Number(_) => 1,
            TypedValue::Boolean(_) => 2,
            TypedValue::Date(_) => 3,
        }
    }

    /// Compares two values under the one rule, reporting what the comparison
    /// had to assume.
    pub fn compare(&self, other: &Self) -> Comparison {
        let signal = match (self, other) {
            (TypedValue::Date(left), TypedValue::Date(right))
                if matches!(left.offset, Offset::Stated(_))
                    != matches!(right.offset, Offset::Stated(_)) =>
            {
                Some(ComparisonSignal::MixedOffset)
            }
            _ => None,
        };
        Comparison {
            ordering: self.cmp(other),
            signal,
        }
    }

    /// A text encoding whose bytewise order is [`Ord`]'s order.
    ///
    /// The leading character is the variant's rank, so the cross-variant order
    /// holds in the encoding too. Numbers and dates are encoded as the 64 bits
    /// that order them, which for a float means flipping the sign bit on a
    /// positive value and every bit on a negative one — the standard order-
    /// preserving map — and for a signed integer means biasing it above zero.
    pub fn sort_key(&self) -> String {
        match self {
            TypedValue::Text(text) => format!("0{text}"),
            TypedValue::Number(number) => format!("1{:016x}", orderable_float(*number)),
            TypedValue::Boolean(flag) => format!("2{}", u8::from(*flag)),
            TypedValue::Date(date) => format!("3{:016x}", orderable_integer(date.seconds)),
        }
    }
}

/// One comparison's answer and what it had to assume to reach it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Comparison {
    /// The order the two values stand in.
    pub ordering: Ordering,
    /// What the comparison assumed, where it assumed anything.
    pub signal: Option<ComparisonSignal>,
}

/// What a comparison had to assume.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ComparisonSignal {
    /// One side stated an offset from UTC and the other did not, so the
    /// unstated side was read at offset zero.
    MixedOffset,
}

impl fmt::Display for ComparisonSignal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ComparisonSignal::MixedOffset => formatter.write_str(
                "one date states an offset from UTC and the other does not, \
                 so the one that does not was read at offset zero",
            ),
        }
    }
}

/// The bit pattern whose unsigned order is `number`'s numeric order.
fn orderable_float(number: f64) -> u64 {
    let bits = number.to_bits();
    if number.is_sign_negative() {
        !bits
    } else {
        bits ^ (1 << 63)
    }
}

/// The bit pattern whose unsigned order is `value`'s signed order.
fn orderable_integer(value: i64) -> u64 {
    (value as u64) ^ (1 << 63)
}

/// Reads a calendar day or an instant, or nothing.
fn read_date(raw: &str) -> Option<DateValue> {
    let (day, rest) = raw.split_at_checked(10)?;
    let days = civil_days(day)?;
    if rest.is_empty() {
        return Some(DateValue {
            seconds: days * 86_400,
            offset: Offset::Unstated,
        });
    }
    let rest = rest.strip_prefix(['T', 't', ' '])?;
    let (clock, zone) = split_zone(rest);
    let seconds_in_day = clock_seconds(clock)?;
    let offset = read_offset(zone)?;
    let applied = match offset {
        Offset::Unstated => 0,
        Offset::Stated(minutes) => i64::from(minutes) * 60,
    };
    Some(DateValue {
        seconds: days * 86_400 + seconds_in_day - applied,
        offset,
    })
}

/// Splits a time from the zone suffix written after it.
fn split_zone(rest: &str) -> (&str, &str) {
    match rest
        .char_indices()
        .find(|(index, ch)| matches!(ch, 'Z' | 'z') || (matches!(ch, '+' | '-') && *index > 0))
    {
        Some((index, _)) => rest.split_at(index),
        None => (rest, ""),
    }
}

/// The number of days from 1970-01-01 to the `YYYY-MM-DD` in `day`.
///
/// Howard Hinnant's `days_from_civil`, which is exact for every proleptic
/// Gregorian date and needs no table.
fn civil_days(day: &str) -> Option<i64> {
    let mut parts = day.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = two_digits(parts.next()?)?;
    let calendar_day: i64 = two_digits(parts.next()?)?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&calendar_day) {
        return None;
    }
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + calendar_day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

/// The seconds into the day that `clock` names, reading `hh:mm[:ss[.fff]]`.
fn clock_seconds(clock: &str) -> Option<i64> {
    let clock = clock.split('.').next()?;
    let mut parts = clock.split(':');
    let hours = two_digits(parts.next()?)?;
    let minutes = two_digits(parts.next()?)?;
    let seconds = match parts.next() {
        Some(field) => two_digits(field)?,
        None => 0,
    };
    if parts.next().is_some() || hours > 23 || minutes > 59 || seconds > 60 {
        return None;
    }
    Some(hours * 3_600 + minutes * 60 + seconds)
}

/// The offset a zone suffix states, or that it states none.
fn read_offset(zone: &str) -> Option<Offset> {
    if zone.is_empty() {
        return Some(Offset::Unstated);
    }
    if matches!(zone, "Z" | "z") {
        return Some(Offset::Stated(0));
    }
    let (sign, rest) = zone.split_at_checked(1)?;
    let sign = match sign {
        "+" => 1,
        "-" => -1,
        _ => return None,
    };
    let (hours, minutes) = match rest.split_once(':') {
        Some((hours, minutes)) => (hours, minutes),
        None if rest.len() == 4 => rest.split_at(2),
        None => (rest, "00"),
    };
    let hours = i32::try_from(two_digits(hours)?).ok()?;
    let minutes = i32::try_from(two_digits(minutes)?).ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(Offset::Stated(sign * (hours * 60 + minutes)))
}

/// A field of ASCII digits, read as a number. A field that is not all digits is
/// not a field of this grammar — `+1` and ` 1` are refusals rather than ones.
fn two_digits(field: &str) -> Option<i64> {
    if field.is_empty() || !field.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    field.parse().ok()
}
