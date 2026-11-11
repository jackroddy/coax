use std::time::{SystemTime, UNIX_EPOCH};

/// A minute of the local wall clock.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Time {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
}

impl Time {
    pub fn now() -> Time {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()) as libc::time_t;
        // SAFETY: libc::tm is plain integers and one pointer, all of which
        // may be zero
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: both pointers are to locals that outlive the call, and
        // localtime_r writes only through the second
        unsafe { libc::localtime_r(&seconds, &mut tm) };
        Time {
            year: tm.tm_year + 1900,
            month: (tm.tm_mon + 1) as u8,
            day: tm.tm_mday as u8,
            hour: tm.tm_hour as u8,
            minute: tm.tm_min as u8,
        }
    }

    /// `HH:MM`, after the date when `today` falls on another day.
    pub fn label(self, today: Time) -> String {
        let clock = format!("{:02}:{:02}", self.hour, self.minute);
        if (self.year, self.month, self.day) == (today.year, today.month, today.day) {
            clock
        } else {
            format!("{}-{:02}-{:02} {clock}", self.year, self.month, self.day)
        }
    }

    /// How long before `now` this was, as `now`, `5m`, `2h` or `3d`.
    pub fn age(self, now: Time) -> String {
        let minutes = now.minutes() - self.minutes();
        if minutes < 1 {
            "now".to_string()
        } else if minutes < 60 {
            format!("{minutes}m")
        } else if minutes < 24 * 60 {
            format!("{}h", minutes / 60)
        } else {
            format!("{}d", minutes / (24 * 60))
        }
    }

    /// The minutes since 1970-01-01 00:00.
    fn minutes(self) -> i64 {
        // the days-from-civil count, with the year starting
        // on 1 March so the leap day falls at its end
        // see: https://howardhinnant.github.io/date_algorithms.html#days_from_civil
        let year = i64::from(self.year) - i64::from(self.month <= 2);
        let era = year.div_euclid(400);
        let year_of_era = year - era * 400;
        let month = (i64::from(self.month) + 9) % 12;
        let day_of_year = (153 * month + 2) / 5 + i64::from(self.day) - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        let days = era * 146_097 + day_of_era - 719_468;
        days * 24 * 60 + i64::from(self.hour) * 60 + i64::from(self.minute)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(year: i32, month: u8, day: u8, hour: u8, minute: u8) -> Time {
        Time {
            year,
            month,
            day,
            hour,
            minute,
        }
    }

    #[test]
    fn an_age_reads_now_then_minutes_then_hours_then_days() {
        let now = at(2026, 11, 10, 12, 30);
        assert_eq!(at(2026, 11, 10, 12, 30).age(now), "now");
        assert_eq!(at(2026, 11, 10, 12, 25).age(now), "5m");
        assert_eq!(at(2026, 11, 10, 10, 29).age(now), "2h");
        assert_eq!(at(2026, 11, 7, 12, 30).age(now), "3d");
    }

    #[test]
    fn an_age_counts_across_midnight_month_ends_and_a_leap_day() {
        assert_eq!(at(2026, 11, 9, 23, 50).age(at(2026, 11, 10, 0, 10)), "20m");
        assert_eq!(at(2026, 10, 31, 23, 0).age(at(2026, 11, 1, 1, 0)), "2h");
        assert_eq!(at(2025, 12, 31, 12, 0).age(at(2026, 1, 1, 12, 0)), "1d");
        assert_eq!(at(2028, 2, 28, 12, 0).age(at(2028, 3, 1, 12, 0)), "2d");
        assert_eq!(at(1970, 1, 1, 0, 1).minutes(), 1);
    }
}
