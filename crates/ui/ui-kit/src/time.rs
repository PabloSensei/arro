//! Relative times of the interface: "just now", "5 minutes ago", "Yesterday", and the
//! short date of a moment older than a week. The date is UTC and has the form
//! `YYYY-MM-DD`.

use std::time::{SystemTime, UNIX_EPOCH};

/// `YYYY-MM-DD` of a moment counted from 1970, in UTC.
pub fn date_of(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let (year, month, day) = civil(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// How long ago `at` was: less than a minute is "just now", then minutes and hours with
/// their plural, the previous day and up to a week as days, and a date after that.
pub fn ago(at: SystemTime, now: SystemTime) -> String {
    let seconds = now.duration_since(at).map_or(0, |age| age.as_secs());
    if seconds < 60 {
        return "just now".to_owned();
    }
    if seconds < 3_600 {
        return count_ago(seconds / 60, "minute");
    }
    if seconds < 86_400 {
        return count_ago(seconds / 3_600, "hour");
    }
    let days = days_of(now).saturating_sub(days_of(at));
    match days {
        0 | 1 => "Yesterday".to_owned(),
        2..=7 => format!("{days} days ago"),
        _ => date_of(seconds_of(at)),
    }
}

/// [`ago`] of a moment against the clock of the machine.
pub fn ago_now(at: SystemTime) -> String {
    ago(at, SystemTime::now())
}

/// Seconds since 1970 of a moment, negative before it.
fn seconds_of(at: SystemTime) -> i64 {
    match at.duration_since(UNIX_EPOCH) {
        Ok(since) => since.as_secs() as i64,
        Err(before) => -(before.duration().as_secs() as i64),
    }
}

/// The day of a moment counted from 1970-01-01, in UTC.
fn days_of(at: SystemTime) -> i64 {
    seconds_of(at).div_euclid(86_400)
}

/// `N minutes ago` or `N hours ago`, with the singular of one.
fn count_ago(count: u64, unit: &str) -> String {
    if count == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{count} {unit}s ago")
    }
}

/// The civil date of the days since 1970-01-01, as the calendar repeats every 400 years.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::ago;

    /// 2023-11-14 22:13:20 UTC.
    const NOW: u64 = 1_700_000_000;

    fn ago_by(seconds: u64) -> String {
        ago(
            UNIX_EPOCH + Duration::from_secs(NOW - seconds),
            UNIX_EPOCH + Duration::from_secs(NOW),
        )
    }

    #[test]
    fn less_than_a_minute_is_just_now() {
        assert_eq!(ago_by(30), "just now");
    }

    #[test]
    fn minutes_and_hours_agree_with_their_number() {
        assert_eq!(ago_by(60), "1 minute ago");
        assert_eq!(ago_by(5 * 60), "5 minutes ago");
        assert_eq!(ago_by(3_600), "1 hour ago");
        assert_eq!(ago_by(3 * 3_600), "3 hours ago");
    }

    #[test]
    fn the_day_before_is_yesterday_and_not_one_day_ago() {
        assert_eq!(ago_by(86_400), "Yesterday");
    }

    #[test]
    fn days_up_to_a_week_carry_their_number() {
        assert_eq!(ago_by(2 * 86_400), "2 days ago");
        assert_eq!(ago_by(3 * 86_400), "3 days ago");
        assert_eq!(ago_by(7 * 86_400), "7 days ago");
    }

    #[test]
    fn an_older_moment_shows_the_short_date() {
        assert_eq!(ago_by(10 * 86_400), "2023-11-04");
    }
}
