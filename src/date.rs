use std::time::{SystemTime, UNIX_EPOCH};

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

// year, month 1-12, day, hour, min, sec, dow 0=Sun for a Unix timestamp.
pub(crate) struct Date {
    y: i64,
    mo: i64,
    d: i64,
    h: u64,
    m: u64,
    s: u64,
    dow: i64,
}

// Uses Howard Hinnant's civil_from_days algorithm.
impl Date {
    pub fn new(secs: u64) -> Self {
        let days = (secs / 86400) as i64;
        let sod = secs % 86400;
        let (h, m, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
        let dow = (days + 4) % 7; // epoch was a Thursday; 0 = Sunday
        let z = days + 719468;
        let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let mo = mp + if mp < 10 { 3 } else { -9 };
        let y = y + if mo <= 2 { 1 } else { 0 };
        Date {
            y,
            mo,
            d,
            h,
            m,
            s,
            dow,
        }
    }

    pub fn now() -> Self {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self::new(secs)
    }

    pub fn http(&self) -> String {
        format!(
            "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
            DAYS[self.dow as usize],
            self.d,
            MONTHS[(self.mo - 1) as usize],
            self.y,
            self.h,
            self.m,
            self.s,
        )
    }

    pub fn clf(&self) -> String {
        format!(
            "[{:02}/{}/{:04}:{:02}:{:02}:{:02} +0000]",
            self.d,
            MONTHS[(self.mo - 1) as usize],
            self.y,
            self.h,
            self.m,
            self.s,
        )
    }
}

#[test]
fn test_http_date() {
    assert_eq!(Date::new(0).http(), "Thu, 01 Jan 1970 00:00:00 GMT");
    assert_eq!(Date::new(86399).http(), "Thu, 01 Jan 1970 23:59:59 GMT");
    assert_eq!(Date::new(86400).http(), "Fri, 02 Jan 1970 00:00:00 GMT");
    assert_eq!(Date::new(951782400).http(), "Tue, 29 Feb 2000 00:00:00 GMT");
    assert_eq!(
        Date::new(1735732800).http(),
        "Wed, 01 Jan 2025 12:00:00 GMT"
    );
}

#[test]
fn test_http_date_random() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/date_test_cases.txt");
    let Ok(data) = std::fs::read_to_string(path) else {
        return;
    };
    for line in data.lines() {
        let (secs, expected) = line.split_once(' ').unwrap();
        assert_eq!(Date::new(secs.parse().unwrap()).http(), expected);
    }
}
