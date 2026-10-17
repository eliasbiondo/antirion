//! Five-field cron expressions in UTC (minute, hour, day of month, month, day of week): `*`,
//! numbers, ranges `a-b`, steps `*/n` and `a-b/n`, and comma lists. Day of week counts from 0
//! (Sunday) and also takes 7 for Sunday. When both day fields are restricted, a minute matches
//! when either does, as in classic cron.

/// A parsed expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cron {
    minutes: u64,
    hours: u64,
    days: u64,
    months: u64,
    weekdays: u64,
    any_day: bool,
    any_weekday: bool,
}

fn number(s: &str, min: u32, max: u32) -> Result<u32, String> {
    let n: u32 = s.parse().map_err(|_| format!("`{s}` is not a number"))?;
    if n < min || n > max {
        return Err(format!("{n} is outside {min}–{max}"));
    }
    Ok(n)
}

/// The bits of one field; `*` alone also reports that the field is unrestricted.
fn field(s: &str, min: u32, max: u32) -> Result<(u64, bool), String> {
    let mut bits = 0u64;
    for part in s.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, st)) => {
                let step = number(st, 1, max.max(1))?;
                (r, step)
            }
            None => (part, 1),
        };
        let (lo, hi) = match range {
            "*" => (min, max),
            r => match r.split_once('-') {
                Some((a, b)) => (number(a, min, max)?, number(b, min, max)?),
                None => {
                    let n = number(r, min, max)?;
                    (n, if part.contains('/') { max } else { n })
                }
            },
        };
        if lo > hi {
            return Err(format!("the range {lo}-{hi} is empty"));
        }
        let mut v = lo;
        while v <= hi {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok((bits, s == "*"))
}

/// Year, month (1–12) and day (1–31) of a day counted from 1970-01-01.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

impl Cron {
    /// Parses an expression; the error says what is wrong.
    pub fn parse(s: &str) -> Result<Cron, String> {
        let fields: Vec<&str> = s.split_whitespace().collect();
        let [m, h, dom, mon, dow] = fields.as_slice() else {
            return Err("a cron expression has five fields".to_owned());
        };
        let (minutes, _) = field(m, 0, 59)?;
        let (hours, _) = field(h, 0, 23)?;
        let (days, any_day) = field(dom, 1, 31)?;
        let (months, _) = field(mon, 1, 12)?;
        let (mut weekdays, any_weekday) = field(dow, 0, 7)?;
        if weekdays & (1 << 7) != 0 {
            weekdays |= 1;
        }
        Ok(Cron {
            minutes,
            hours,
            days,
            months,
            weekdays,
            any_day,
            any_weekday,
        })
    }

    /// Whether the UTC minute starting `unix_minute` minutes after the epoch matches.
    #[must_use]
    pub fn matches(&self, unix_minute: i64) -> bool {
        let days = unix_minute.div_euclid(1440);
        let in_day = unix_minute.rem_euclid(1440);
        let (minute, hour) = (in_day % 60, in_day / 60);
        let (_, month, day) = civil(days);
        let weekday = (days + 4).rem_euclid(7);
        let has = |bits: u64, v: i64| u32::try_from(v).is_ok_and(|v| v < 64 && bits & (1 << v) != 0);
        let day_ok = match (self.any_day, self.any_weekday) {
            (true, true) => true,
            (true, false) => has(self.weekdays, weekday),
            (false, true) => has(self.days, i64::from(day)),
            (false, false) => has(self.days, i64::from(day)) || has(self.weekdays, weekday),
        };
        has(self.minutes, minute) && has(self.hours, hour) && has(self.months, i64::from(month)) && day_ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minutes since the epoch of a UTC time.
    fn at(days: i64, hour: i64, minute: i64) -> i64 {
        days * 1440 + hour * 60 + minute
    }

    #[test]
    fn parses_and_matches() {
        let nightly = Cron::parse("0 3 * * *").unwrap();
        assert!(nightly.matches(at(20_000, 3, 0)));
        assert!(!nightly.matches(at(20_000, 3, 1)));
        let every_15 = Cron::parse("*/15 * * * *").unwrap();
        assert!(every_15.matches(at(1, 2, 45)) && !every_15.matches(at(1, 2, 46)));
        let weekdays = Cron::parse("30 9 * * 1-5").unwrap();
        assert!(weekdays.matches(at(4, 9, 30)), "1970-01-05 was a Monday");
        assert!(!weekdays.matches(at(3, 9, 30)), "1970-01-04 was a Sunday");
        let sunday = Cron::parse("0 0 * * 7").unwrap();
        assert!(sunday.matches(at(3, 0, 0)));
        let first = Cron::parse("0 0 1 3 *").unwrap();
        assert!(first.matches(at(59, 0, 0)), "1970-03-01 is day 59");
        assert_eq!(civil(19_723), (2024, 1, 1));
    }

    #[test]
    fn refuses_what_is_not_cron() {
        for bad in [
            "",
            "* * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "5-1 * * * *",
            "*/0 * * * *",
            "a * * * *",
        ] {
            assert!(Cron::parse(bad).is_err(), "{bad}");
        }
    }
}
