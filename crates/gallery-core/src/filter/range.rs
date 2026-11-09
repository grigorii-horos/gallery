//! Parsers for numeric ranges, durations, times of day and date ranges.

use crate::{Error, Result};
use chrono::{Duration, NaiveDate, NaiveTime};

/// What the numbers of a range mean; decides the accepted suffixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Plain,
    /// `B`, `KB`, `MB`, `GB`, `TB` (powers of 1024).
    Bytes,
    /// `s`, `m`, `h` (result in seconds).
    Seconds,
}

/// A numeric interval with open or closed ends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumRange {
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub min_incl: bool,
    pub max_incl: bool,
}

fn bad_range(s: &str) -> Error {
    Error::Config(crate::t!("filter.bad_range", value = s))
}

fn parse_num(s: &str, unit: Unit) -> Option<f64> {
    let s = s.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, suffix) = s.split_at(split);
    let n: f64 = num.parse().ok().filter(|n: &f64| n.is_finite())?;
    let suffix = suffix.trim().to_ascii_lowercase();
    let mult = match (unit, suffix.as_str()) {
        (_, "") => 1.0,
        (Unit::Bytes, "b") => 1.0,
        (Unit::Bytes, "kb") => 1024.0,
        (Unit::Bytes, "mb") => 1024.0 * 1024.0,
        (Unit::Bytes, "gb") => 1024.0 * 1024.0 * 1024.0,
        (Unit::Bytes, "tb") => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        (Unit::Seconds, "s") => 1.0,
        (Unit::Seconds, "m") => 60.0,
        (Unit::Seconds, "h") => 3600.0,
        _ => return None,
    };
    Some(n * mult)
}

impl NumRange {
    /// Forms: `a..b`, `..b`, `a..` (ends inclusive), `>a`, `>=a`, `<b`,
    /// `<=b`, or a bare number (exact value).
    pub fn parse(s: &str, unit: Unit) -> Result<NumRange> {
        let t = s.trim();
        let num = |x: &str| parse_num(x, unit).ok_or_else(|| bad_range(s));
        let mut r = NumRange {
            min: None,
            max: None,
            min_incl: true,
            max_incl: true,
        };
        if let Some((a, b)) = t.split_once("..") {
            if a.trim().is_empty() && b.trim().is_empty() {
                return Err(bad_range(s));
            }
            if !a.trim().is_empty() {
                r.min = Some(num(a)?);
            }
            if !b.trim().is_empty() {
                r.max = Some(num(b)?);
            }
        } else if let Some(x) = t.strip_prefix(">=") {
            r.min = Some(num(x)?);
        } else if let Some(x) = t.strip_prefix("<=") {
            r.max = Some(num(x)?);
        } else if let Some(x) = t.strip_prefix('>') {
            r.min = Some(num(x)?);
            r.min_incl = false;
        } else if let Some(x) = t.strip_prefix('<') {
            r.max = Some(num(x)?);
            r.max_incl = false;
        } else {
            let v = num(t)?;
            r.min = Some(v);
            r.max = Some(v);
        }
        if let (Some(lo), Some(hi)) = (r.min, r.max)
            && lo > hi
        {
            return Err(bad_range(s));
        }
        Ok(r)
    }

    pub fn contains(&self, v: f64) -> bool {
        let lo = match self.min {
            Some(m) => v > m || (self.min_incl && v == m),
            None => true,
        };
        let hi = match self.max {
            Some(m) => v < m || (self.max_incl && v == m),
            None => true,
        };
        lo && hi
    }
}

/// `5s`, `10m`, `2h`, `30d`, `1w`, `1y` (365 days).
pub fn parse_duration(s: &str) -> Result<Duration> {
    let bad = || Error::Config(crate::t!("filter.bad_duration", value = s));
    let t = s.trim();
    let split = t.find(|c: char| !c.is_ascii_digit()).ok_or_else(bad)?;
    let (num, unit) = t.split_at(split);
    let n: i64 = num.parse().map_err(|_| bad())?;
    let secs = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        "y" => 365 * 86_400,
        _ => return Err(bad()),
    };
    n.checked_mul(secs)
        .and_then(Duration::try_seconds)
        .ok_or_else(bad)
}

/// A daily window `from..to`; wraps midnight when `from > to`.
/// `from` is inclusive, `to` exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeOfDay {
    pub from: NaiveTime,
    pub to: NaiveTime,
}

impl TimeOfDay {
    pub fn parse(s: &str) -> Result<TimeOfDay> {
        let bad = || Error::Config(crate::t!("filter.bad_time", value = s));
        let (a, b) = s.split_once("..").ok_or_else(bad)?;
        let t = |x: &str| NaiveTime::parse_from_str(x.trim(), "%H:%M").map_err(|_| bad());
        Ok(TimeOfDay {
            from: t(a)?,
            to: t(b)?,
        })
    }

    pub fn contains(&self, t: NaiveTime) -> bool {
        if self.from <= self.to {
            t >= self.from && t < self.to
        } else {
            t >= self.from || t < self.to
        }
    }
}

/// Inclusive date interval; either end may be open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateRange {
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
}

impl DateRange {
    pub fn parse(s: &str) -> Result<DateRange> {
        let bad = || Error::Config(crate::t!("filter.bad_date", value = s));
        let (a, b) = s.split_once("..").ok_or_else(bad)?;
        let d = |x: &str| -> Result<Option<NaiveDate>> {
            let x = x.trim();
            if x.is_empty() {
                return Ok(None);
            }
            NaiveDate::parse_from_str(x, "%Y-%m-%d")
                .map(Some)
                .map_err(|_| bad())
        };
        let r = DateRange {
            from: d(a)?,
            to: d(b)?,
        };
        match (r.from, r.to) {
            (None, None) => Err(bad()),
            (Some(f), Some(t)) if f > t => Err(bad()),
            _ => Ok(r),
        }
    }

    pub fn contains(&self, d: NaiveDate) -> bool {
        self.from.is_none_or(|f| d >= f) && self.to.is_none_or(|t| d <= t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KB: f64 = 1024.0;

    #[test]
    fn bytes_upper_bound() {
        let r = NumRange::parse("..500KB", Unit::Bytes).unwrap();
        assert!(r.contains(500.0 * KB));
        assert!(!r.contains(500.0 * KB + 1.0));
        let r = NumRange::parse("..2GB", Unit::Bytes).unwrap();
        assert!(r.contains(2.0 * KB * KB * KB));
    }

    #[test]
    fn bytes_between() {
        let r = NumRange::parse("1MB..20MB", Unit::Bytes).unwrap();
        assert!(r.contains(KB * KB));
        assert!(r.contains(20.0 * KB * KB));
        assert!(!r.contains(KB * KB - 1.0));
        assert!(!r.contains(20.0 * KB * KB + 1.0));
    }

    #[test]
    fn comparison_forms() {
        let r = NumRange::parse(">=60", Unit::Plain).unwrap();
        assert!(r.contains(60.0));
        assert!(!r.contains(59.9));
        let r = NumRange::parse("<40", Unit::Plain).unwrap();
        assert!(r.contains(39.0));
        assert!(!r.contains(40.0));
        let r = NumRange::parse(">40", Unit::Plain).unwrap();
        assert!(!r.contains(40.0));
        assert!(r.contains(41.0));
        let r = NumRange::parse("5", Unit::Plain).unwrap();
        assert!(r.contains(5.0));
        assert!(!r.contains(6.0));
    }

    #[test]
    fn seconds() {
        let r = NumRange::parse("..10s", Unit::Seconds).unwrap();
        assert!(r.contains(10.0));
        assert!(!r.contains(10.5));
        let r = NumRange::parse("1m..", Unit::Seconds).unwrap();
        assert!(r.contains(60.0));
        assert!(!r.contains(59.0));
    }

    #[test]
    fn range_errors_name_input() {
        for (s, u) in [
            ("16:9", Unit::Plain),
            ("..", Unit::Plain),
            ("abc", Unit::Plain),
            ("5..1", Unit::Plain),
            ("10KB", Unit::Plain),
            ("10s", Unit::Bytes),
            ("1x..2", Unit::Bytes),
        ] {
            let e = NumRange::parse(s, u).unwrap_err().to_string();
            assert!(e.contains(s), "{e}");
        }
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("5s").unwrap(), Duration::seconds(5));
        assert_eq!(parse_duration("2h").unwrap(), Duration::hours(2));
        assert_eq!(parse_duration("30d").unwrap(), Duration::days(30));
        assert_eq!(parse_duration("1y").unwrap(), Duration::days(365));
        assert_eq!(parse_duration("1w").unwrap(), Duration::days(7));
        for s in ["", "5", "h", "5x", "-5s", "1.5h"] {
            let e = parse_duration(s).unwrap_err().to_string();
            assert!(e.contains(s), "{e}");
        }
    }

    fn tm(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn time_of_day_wraps_midnight() {
        let t = TimeOfDay::parse("22:00..05:00").unwrap();
        assert!(t.contains(tm(23, 30)));
        assert!(t.contains(tm(4, 59)));
        assert!(t.contains(tm(22, 0)));
        assert!(!t.contains(tm(5, 0)));
        assert!(!t.contains(tm(12, 0)));
        let d = TimeOfDay::parse("09:00..17:00").unwrap();
        assert!(d.contains(tm(9, 0)));
        assert!(!d.contains(tm(17, 0)));
        for s in ["22:00", "25:00..05:00", "a..b"] {
            let e = TimeOfDay::parse(s).unwrap_err().to_string();
            assert!(e.contains(s), "{e}");
        }
    }

    #[test]
    fn date_range() {
        let ymd = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();
        let r = DateRange::parse("2026-08-12..2026-08-18").unwrap();
        assert!(r.contains(ymd(2026, 8, 12)));
        assert!(r.contains(ymd(2026, 8, 18)));
        assert!(!r.contains(ymd(2026, 8, 19)));
        let r = DateRange::parse("2026-01-01..").unwrap();
        assert!(r.contains(ymd(2030, 1, 1)));
        assert!(!r.contains(ymd(2025, 12, 31)));
        let r = DateRange::parse("..2026-01-01").unwrap();
        assert!(r.contains(ymd(2000, 1, 1)));
        for s in ["2026-01-01", "..", "2026-13-01..", "2026-02-01..2026-01-01"] {
            let e = DateRange::parse(s).unwrap_err().to_string();
            assert!(e.contains(s), "{e}");
        }
    }
}
