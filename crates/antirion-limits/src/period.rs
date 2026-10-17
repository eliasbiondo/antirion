//! Budget periods: calendar periods in a time zone, epoch-aligned durations, and `lifetime`.

use antirion_core::UtcMicros;
use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan, Zoned};

/// A calendar unit.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Calendar {
    Daily,
    Weekly,
    Monthly,
    Quarterly,
    Yearly,
}

/// How a budget's periods are cut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeriodSpec {
    /// One period that never ends.
    Lifetime,
    /// Half-open intervals between local midnights in `tz`.
    Calendar {
        unit: Calendar,
        tz: TimeZone,
        week_start: Weekday,
    },
    /// `[k·d, (k+1)·d)` from the Unix epoch, in microseconds.
    Every { us: i64 },
}

/// The period containing an instant: `[start, end)`, `end` absent for `lifetime`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct PeriodBounds {
    pub start: UtcMicros,
    pub end: Option<UtcMicros>,
}

/// A period definition that cannot be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PeriodError {
    #[error("unknown time zone {0:?}")]
    TimeZone(String),
    #[error("unknown period {0:?}")]
    Period(String),
    #[error("unknown week day {0:?}")]
    Weekday(String),
}

fn weekday(s: &str) -> Result<Weekday, PeriodError> {
    Ok(match s {
        "monday" => Weekday::Monday,
        "tuesday" => Weekday::Tuesday,
        "wednesday" => Weekday::Wednesday,
        "thursday" => Weekday::Thursday,
        "friday" => Weekday::Friday,
        "saturday" => Weekday::Saturday,
        "sunday" => Weekday::Sunday,
        other => return Err(PeriodError::Weekday(other.to_owned())),
    })
}

impl PeriodSpec {
    /// Reads `period`, `timezone` and `week_start` as a budget declares them.
    pub fn parse(
        period: &str,
        timezone: Option<&str>,
        week_start: Option<&str>,
    ) -> Result<PeriodSpec, PeriodError> {
        let unit = match period {
            "lifetime" => return Ok(PeriodSpec::Lifetime),
            "daily" => Calendar::Daily,
            "weekly" => Calendar::Weekly,
            "monthly" => Calendar::Monthly,
            "quarterly" => Calendar::Quarterly,
            "yearly" => Calendar::Yearly,
            other => {
                let nanos = antirion_config::scalar::duration_nanos(other)
                    .ok_or_else(|| PeriodError::Period(other.to_owned()))?;
                let us = i64::try_from(nanos / 1_000)
                    .ok()
                    .filter(|us| *us > 0)
                    .ok_or_else(|| PeriodError::Period(other.to_owned()))?;
                return Ok(PeriodSpec::Every { us });
            }
        };
        let name = timezone.unwrap_or("UTC");
        let tz = TimeZone::get(name).map_err(|_| PeriodError::TimeZone(name.to_owned()))?;
        let week_start = weekday(week_start.unwrap_or("monday"))?;
        Ok(PeriodSpec::Calendar { unit, tz, week_start })
    }

    /// The period containing `t`.
    #[must_use]
    pub fn bounds(&self, t: UtcMicros) -> PeriodBounds {
        match self {
            PeriodSpec::Lifetime => PeriodBounds {
                start: UtcMicros::EPOCH,
                end: None,
            },
            PeriodSpec::Every { us } => {
                let start = t.0.div_euclid(*us).saturating_mul(*us);
                PeriodBounds {
                    start: UtcMicros(start),
                    end: Some(UtcMicros(start.saturating_add(*us))),
                }
            }
            PeriodSpec::Calendar { unit, tz, week_start } => calendar_bounds(*unit, tz, *week_start, t)
                .unwrap_or(PeriodBounds {
                    start: UtcMicros::EPOCH,
                    end: None,
                }),
        }
    }
}

fn first_day(unit: Calendar, week_start: Weekday, d: Date) -> Option<Date> {
    Some(match unit {
        Calendar::Daily => d,
        Calendar::Weekly => {
            let back = i64::from(d.weekday().since(week_start));
            d.checked_sub(back.days()).ok()?
        }
        Calendar::Monthly => d.first_of_month(),
        Calendar::Quarterly => {
            let month = (d.month() - 1) / 3 * 3 + 1;
            Date::new(d.year(), month, 1).ok()?
        }
        Calendar::Yearly => Date::new(d.year(), 1, 1).ok()?,
    })
}

fn next_first_day(unit: Calendar, start: Date) -> Option<Date> {
    match unit {
        Calendar::Daily => start.checked_add(1.day()).ok(),
        Calendar::Weekly => start.checked_add(7.days()).ok(),
        Calendar::Monthly => start.checked_add(1.month()).ok(),
        Calendar::Quarterly => start.checked_add(3.months()).ok(),
        Calendar::Yearly => start.checked_add(1.year()).ok(),
    }
}

/// The first instant of local day `d` in `tz`. A midnight that does not exist (a gap) becomes the
/// first valid instant after it; one that occurs twice resolves to its first occurrence.
fn midnight(tz: &TimeZone, d: Date) -> Option<Timestamp> {
    d.to_zoned(tz.clone()).ok().map(|z| z.timestamp())
}

fn calendar_bounds(unit: Calendar, tz: &TimeZone, week_start: Weekday, t: UtcMicros) -> Option<PeriodBounds> {
    let ts = Timestamp::from_microsecond(t.0).ok()?;
    let local: Zoned = ts.to_zoned(tz.clone());
    let mut start_day = first_day(unit, week_start, local.date())?;
    let mut start = midnight(tz, start_day)?;
    if start > ts {
        start_day = first_day(unit, week_start, start_day.checked_sub(1.day()).ok()?)?;
        start = midnight(tz, start_day)?;
    }
    let end = midnight(tz, next_first_day(unit, start_day)?)?;
    Some(PeriodBounds {
        start: UtcMicros(start.as_microsecond()),
        end: Some(UtcMicros(end.as_microsecond())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> UtcMicros {
        UtcMicros::parse_rfc3339(s).unwrap()
    }

    fn b(spec: &PeriodSpec, t: &str) -> (String, Option<String>) {
        let p = spec.bounds(at(t));
        (p.start.to_rfc3339(), p.end.map(UtcMicros::to_rfc3339))
    }

    #[test]
    fn utc_calendar_periods() {
        let monthly = PeriodSpec::parse("monthly", None, None).unwrap();
        assert_eq!(
            b(&monthly, "2026-09-30T12:00:00Z"),
            ("2026-09-01T00:00:00Z".into(), Some("2026-10-01T00:00:00Z".into()))
        );
        let q = PeriodSpec::parse("quarterly", None, None).unwrap();
        assert_eq!(
            b(&q, "2026-08-15T00:00:00Z"),
            ("2026-07-01T00:00:00Z".into(), Some("2026-10-01T00:00:00Z".into()))
        );
        let y = PeriodSpec::parse("yearly", None, None).unwrap();
        assert_eq!(
            b(&y, "2026-12-31T23:59:59Z").1,
            Some("2027-01-01T00:00:00Z".into())
        );
        let daily = PeriodSpec::parse("daily", Some("UTC"), None).unwrap();
        assert_eq!(
            b(&daily, "2026-09-30T00:00:00Z"),
            ("2026-09-30T00:00:00Z".into(), Some("2026-10-01T00:00:00Z".into()))
        );
    }

    #[test]
    fn week_start_is_honoured() {
        let mon = PeriodSpec::parse("weekly", None, None).unwrap();
        assert_eq!(b(&mon, "2026-10-01T10:00:00Z").0, "2026-09-28T00:00:00Z");
        let sun = PeriodSpec::parse("weekly", None, Some("sunday")).unwrap();
        assert_eq!(b(&sun, "2026-10-01T10:00:00Z").0, "2026-09-27T00:00:00Z");
        let thu = PeriodSpec::parse("weekly", None, Some("thursday")).unwrap();
        assert_eq!(b(&thu, "2026-10-01T00:00:00Z").0, "2026-10-01T00:00:00Z");
    }

    #[test]
    fn local_midnights_in_a_zone() {
        let berlin = PeriodSpec::parse("daily", Some("Europe/Berlin"), None).unwrap();
        assert_eq!(
            b(&berlin, "2026-09-30T21:59:59Z"),
            ("2026-09-29T22:00:00Z".into(), Some("2026-09-30T22:00:00Z".into()))
        );
        assert_eq!(b(&berlin, "2026-09-30T22:00:00Z").0, "2026-09-30T22:00:00Z");
        assert!(PeriodSpec::parse("daily", Some("Mars/Olympus"), None).is_err());
    }

    #[test]
    fn missing_midnight_starts_at_the_first_valid_instant() {
        // Santiago moved its clocks forward at local midnight on 2026-09-06.
        let s = PeriodSpec::parse("daily", Some("America/Santiago"), None).unwrap();
        let p = s.bounds(at("2026-09-06T12:00:00Z"));
        assert_eq!(p.start.to_rfc3339(), "2026-09-06T04:00:00Z");
        assert!(p.end.unwrap().0 - p.start.0 == 23 * 3_600_000_000);
    }

    #[test]
    fn durations_are_epoch_aligned() {
        let ten = PeriodSpec::parse("10s", None, None).unwrap();
        assert_eq!(
            b(&ten, "2026-09-30T12:00:07Z"),
            ("2026-09-30T12:00:00Z".into(), Some("2026-09-30T12:00:10Z".into()))
        );
        let week = PeriodSpec::parse("7d", None, None).unwrap();
        assert_eq!(b(&week, "1970-01-02T00:00:00Z").0, "1970-01-01T00:00:00Z");
        assert_eq!(
            PeriodSpec::parse("lifetime", None, None)
                .unwrap()
                .bounds(UtcMicros(5))
                .end,
            None
        );
        assert!(PeriodSpec::parse("0s", None, None).is_err());
        assert!(PeriodSpec::parse("abc", None, None).is_err());
    }
}
