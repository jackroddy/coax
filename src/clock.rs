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
}
