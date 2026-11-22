//! When a schedule entry fires next.
//!
//! Two forms: a 5-field cron expression evaluated in local time (`30 8 * * 1-5`), or a fixed
//! interval (`every 30m`). Intervals are anchored to the Unix epoch, so `every 1h` fires on the
//! hour and a daemon restart doesn't shift it.

use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Local, TimeZone};

#[derive(Debug, Clone)]
pub enum Schedule {
    Cron(Box<croner::Cron>),
    Every(Duration),
}

impl Schedule {
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if let Some(rest) = text.strip_prefix("every") {
            return parse_interval(rest.trim()).map(Schedule::Every);
        }
        if text.split_whitespace().count() != 5 {
            return Err(
                "expected 5 cron fields (min hour day month weekday) or `every <n><s|m|h|d>`"
                    .into(),
            );
        }
        croner::Cron::from_str(text)
            .map(|c| Schedule::Cron(Box::new(c)))
            .map_err(|e| e.to_string())
    }

    /// The first time strictly after `after`.
    pub fn next_after<Tz: TimeZone>(&self, after: &DateTime<Tz>) -> Option<DateTime<Tz>> {
        match self {
            Schedule::Cron(cron) => cron.find_next_occurrence(after, false).ok(),
            Schedule::Every(period) => {
                let period_ms = i64::try_from(period.as_millis()).ok()?;
                let now_ms = after.timestamp_millis();
                let next_ms = (now_ms.div_euclid(period_ms) + 1) * period_ms;
                after.timezone().timestamp_millis_opt(next_ms).single()
            }
        }
    }

    pub fn next_local(&self) -> Option<DateTime<Local>> {
        self.next_after(&Local::now())
    }
}

fn parse_interval(text: &str) -> Result<Duration, String> {
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| "missing unit (s, m, h or d)".to_string())?;
    let (number, unit) = text.split_at(split);
    let n: u64 = number
        .parse()
        .map_err(|_| format!("`{text}` is not `<number><unit>`"))?;
    let seconds = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => return Err(format!("unknown unit `{unit}`")),
    };
    if seconds < 60 {
        return Err("intervals shorter than 1 minute are not allowed".into());
    }
    Ok(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, FixedOffset, Timelike, Weekday};

    fn kst(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(9 * 3600)
            .unwrap()
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
    }

    #[test]
    fn weekday_cron_skips_weekend() {
        let s = Schedule::parse("30 8 * * 1-5").unwrap();
        // 2026-11-20 is a Friday; after 09:00 the next run is Monday 08:30.
        let next = s.next_after(&kst(2026, 11, 20, 9, 0)).unwrap();
        assert_eq!(next.weekday(), Weekday::Mon);
        assert_eq!((next.hour(), next.minute()), (8, 30));
    }

    #[test]
    fn cron_is_strictly_after() {
        let s = Schedule::parse("30 8 * * *").unwrap();
        let at = kst(2026, 11, 23, 8, 30);
        assert_eq!(s.next_after(&at).unwrap().day(), 24);
    }

    #[test]
    fn cron_in_local_time_is_in_the_future() {
        let s = Schedule::parse("*/5 * * * *").unwrap();
        let next = s.next_local().unwrap();
        let now = Local::now();
        assert!(next > now && next - now <= chrono::Duration::minutes(5));
    }

    #[test]
    fn every_is_epoch_anchored() {
        let s = Schedule::parse("every 1h").unwrap();
        let next = s.next_after(&kst(2026, 11, 23, 8, 17)).unwrap();
        assert_eq!((next.hour(), next.minute()), (9, 0));
        let s = Schedule::parse("every 30m").unwrap();
        let next = s.next_after(&kst(2026, 11, 23, 8, 30)).unwrap();
        assert_eq!((next.hour(), next.minute()), (9, 0));
    }

    #[test]
    fn rejects_bad_input() {
        for bad in [
            "every",
            "every 10",
            "every 5x",
            "every 10s",
            "* * *",
            "61 * * * *",
            "every -1m",
        ] {
            assert!(Schedule::parse(bad).is_err(), "{bad} should be rejected");
        }
    }
}
