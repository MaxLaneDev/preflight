//! Numeric five-field cron, evaluated in an explicit IANA time zone.
use crate::model::invalid;
use anyhow::{Result, ensure};
use jiff::{Timestamp, ToSpan, tz::TimeZone};

struct Field {
    values: Vec<u8>,
    wildcard: bool,
}
fn field(text: &str, min: u8, max: u8) -> Result<Field> {
    let mut values = Vec::new();
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => (
                range,
                step.parse::<u8>()
                    .map_err(|_| invalid("invalid cron step"))?,
            ),
            None => (part, 1),
        };
        ensure!(step > 0, invalid("cron step must be positive"));
        let number = |n: &str| -> Result<u8> {
            let v = n
                .parse::<u8>()
                .map_err(|_| invalid("cron fields must be numeric"))?;
            ensure!((min..=max).contains(&v), invalid("cron field out of range"));
            Ok(v)
        };
        let (start, end) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (number(a)?, number(b)?)
        } else {
            let n = number(range)?;
            (n, if part.contains('/') { max } else { n })
        };
        ensure!(start <= end, invalid("reversed cron range"));
        values.extend((start..=end).step_by(step as usize));
    }
    values.sort_unstable();
    values.dedup();
    Ok(Field {
        values,
        wildcard: text.starts_with('*'),
    })
}

pub fn next(cron: &str, timezone: &str, after: i64) -> Result<i64> {
    ensure!(
        cron.len() <= 256 && !timezone.is_empty() && timezone.len() <= 128,
        invalid("bounded cron and IANA timezone required")
    );
    let fields = cron.split_whitespace().collect::<Vec<_>>();
    ensure!(
        fields.len() == 5,
        invalid("cron requires minute hour day-of-month month day-of-week")
    );
    let minute = field(fields[0], 0, 59)?;
    let hour = field(fields[1], 0, 23)?;
    let dom = field(fields[2], 1, 31)?;
    let month = field(fields[3], 1, 12)?;
    let dow = field(fields[4], 0, 7)?;
    let tz = TimeZone::get(timezone).map_err(|_| invalid("unknown IANA timezone"))?;
    let mut date = Timestamp::from_millisecond(after)
        .map_err(|_| invalid("timestamp out of range"))?
        .to_zoned(tz.clone())
        .date();
    for _ in 0..(366 * 8) {
        let day_match = dom.values.contains(&(date.day() as u8));
        let weekday = date.weekday().to_sunday_zero_offset() as u8;
        let week_match = dow.values.contains(&weekday) || (weekday == 0 && dow.values.contains(&7));
        let matches_day = if dom.wildcard || dow.wildcard {
            day_match && week_match
        } else {
            day_match || week_match
        };
        if month.values.contains(&(date.month() as u8)) && matches_day {
            for &h in &hour.values {
                for &m in &minute.values {
                    let civil = date.at(h as i8, m as i8, 0, 0);
                    let zoned = tz.to_ambiguous_zoned(civil).earlier()?;
                    // Skip nonexistent spring-forward times; run repeated wall times once, at the earlier offset.
                    if zoned.datetime() != civil {
                        continue;
                    }
                    let candidate = zoned.timestamp().as_millisecond();
                    if candidate > after {
                        return Ok(candidate);
                    }
                }
            }
        }
        date = date.checked_add(1.days())?;
    }
    Err(invalid("cron has no occurrence within eight years"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ms(s: &str) -> i64 {
        s.parse::<Timestamp>().unwrap().as_millisecond()
    }
    #[test]
    fn numeric_fields_and_calendar_boundaries() {
        assert_eq!(
            next("*/15 9-17 * * 1-5", "UTC", ms("2026-10-30T17:59:00Z")).unwrap(),
            ms("2026-11-02T09:00:00Z")
        );
        assert_eq!(
            next("0 12 29 2 *", "UTC", ms("2025-01-01T00:00:00Z")).unwrap(),
            ms("2028-02-29T12:00:00Z")
        );
        assert_eq!(
            next("0 0 * * 7", "UTC", ms("2026-10-30T00:00:00Z")).unwrap(),
            ms("2026-11-01T00:00:00Z")
        );
        assert_eq!(
            next(
                "0 9 * * *",
                "Asia/Yekaterinburg",
                ms("2026-10-29T00:00:00Z")
            )
            .unwrap(),
            ms("2026-10-29T04:00:00Z")
        );
        for expr in [
            "* * *",
            "60 * * * *",
            "*/0 * * * *",
            "0 0 31 2 *",
            "0 22-1 * * *",
        ] {
            assert!(
                next(expr, "UTC", ms("2026-01-01T00:00:00Z")).is_err(),
                "{expr}"
            );
        }
        assert!(next("* * * * *", "not/a-zone", ms("2026-01-01T00:00:00Z")).is_err());
    }
    #[test]
    fn dst_gap_skipped_fold_runs_once() {
        assert_eq!(
            next("30 2 * * *", "America/New_York", ms("2026-03-08T05:00:00Z")).unwrap(),
            ms("2026-03-09T06:30:00Z")
        );
        let first = next("30 1 * * *", "America/New_York", ms("2026-11-01T04:00:00Z")).unwrap();
        assert_eq!(first, ms("2026-11-01T05:30:00Z"));
        assert_eq!(
            next("30 1 * * *", "America/New_York", first).unwrap(),
            ms("2026-11-02T06:30:00Z")
        );
    }
}
