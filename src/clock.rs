//! The clock, and the dates a task carries. Events are stamped in UTC by
//! `commit::now`; this module is for the one place a task compares a date
//! against the present: `after` (not workable before) and a `due` that is
//! a date (overdue). A date alone, `2026-11-10`, means the start of that
//! day in the reading machine's local time. A time, `2026-11-10T09:00`,
//! is local when entered and is stored with its UTC offset,
//! `2026-11-10T09:00-05:00`, so another machine reads the same moment.
//! The present is read once per command (`now`): `TSK_NOW` if set, in
//! either form, else the system clock with the machine's local offset.
//! Nothing in the derived layer reads the clock: `Derived::compute`
//! takes the `Moment` its caller passes, and only readiness depends on
//! it; statuses and the event log never do.
//!
//! The local offset comes from the C runtime (`localtime_r`, `mktime`)
//! on Unix and from kernel32 (`GetLocalTime`,
//! `TzSpecificLocalTimeToSystemTime`) on Windows, declared here without
//! a crate: both are already linked by the standard library, and the
//! release builds stay pure Rust with no C toolchain.

use anyhow::{bail, Result};

/// The present, as a local calendar time with its instant in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moment {
    /// Local date and time, to the minute.
    pub y: i64,
    pub m: u32,
    pub d: u32,
    pub hh: u32,
    pub mm: u32,
    /// Minutes since the epoch, UTC.
    pub utc_min: i64,
}

/// A stored date or time, parsed for comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// `YYYY-MM-DD`: the start of that local day.
    Date(i64, u32, u32),
    /// `YYYY-MM-DDTHH:MM±HH:MM`: one instant, as minutes since the epoch.
    Time(i64),
}

impl Moment {
    /// The gate still holds: now is before `w`.
    pub fn before(&self, w: &When) -> bool {
        match *w {
            When::Date(y, m, d) => (self.y, self.m, self.d) < (y, m, d),
            When::Time(t) => self.utc_min < t,
        }
    }
    /// Past it: now is after `w` (a date is past once its day is over).
    pub fn past(&self, w: &When) -> bool {
        match *w {
            When::Date(y, m, d) => (self.y, self.m, self.d) > (y, m, d),
            When::Time(t) => self.utc_min > t,
        }
    }
}

/// The present: `TSK_NOW` if set (a date, a local time, or a time with
/// its offset), else the clock.
pub fn now() -> Result<Moment> {
    if let Ok(s) = std::env::var("TSK_NOW") {
        if !s.trim().is_empty() {
            let (y, m, d, hh, mm, off) = match parse_parts(s.trim()) {
                Some(p) => p,
                None => bail!("TSK_NOW wants a date (YYYY-MM-DD) or a local time (YYYY-MM-DDTHH:MM), not `{}`", s.trim()),
            };
            let (hh, mm) = (hh.unwrap_or(0), mm.unwrap_or(0));
            let off = off.unwrap_or_else(|| local_offset_at(y, m, d, hh, mm));
            return Ok(Moment { y, m, d, hh, mm, utc_min: naive_minutes(y, m, d, hh, mm) - off as i64 });
        }
    }
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (y, m, d, hh, mm) = local_now(secs);
    Ok(Moment { y, m, d, hh, mm, utc_min: secs.div_euclid(60) })
}

/// Normalize what a person typed for `--after`, `--due` or `TSK_NOW` to
/// the stored form: a date stays a date; a local time gains the local
/// zone's offset at that moment; a time with an offset is kept.
pub fn normalize(s: &str, what: &str) -> Result<String> {
    let s = s.trim();
    let Some((y, m, d, hh, mm, off)) = parse_parts(s) else {
        bail!("{} wants a date (YYYY-MM-DD) or a local time (YYYY-MM-DDTHH:MM), not `{}`", what, s);
    };
    Ok(match (hh, mm) {
        (None, _) => format!("{:04}-{:02}-{:02}", y, m, d),
        (Some(hh), Some(mm)) => {
            let off = off.unwrap_or_else(|| local_offset_at(y, m, d, hh, mm));
            let sign = if off < 0 { '-' } else { '+' };
            let a = off.abs();
            format!("{:04}-{:02}-{:02}T{:02}:{:02}{}{:02}:{:02}", y, m, d, hh, mm, sign, a / 60, a % 60)
        }
        _ => unreachable!(),
    })
}

/// A `--due` is free text; one that reads as a date or time is stored in
/// the normalized form so the views can compare it.
pub fn normalize_due(s: &str) -> String {
    normalize(s, "--due").unwrap_or_else(|_| s.to_string())
}

/// A stored `after` or `due`, when it is a date or time.
pub fn parse(s: &str) -> Option<When> {
    let (y, m, d, hh, mm, off) = parse_parts(s.trim())?;
    Some(match (hh, mm) {
        (None, _) => When::Date(y, m, d),
        (Some(hh), Some(mm)) => When::Time(naive_minutes(y, m, d, hh, mm) - off.unwrap_or(0) as i64),
        _ => return None,
    })
}

/// `YYYY-MM-DD`, optionally `THH:MM`, optionally `Z` or `±HH:MM`.
/// Returns (y, m, d, hh, mm, offset minutes).
fn parse_parts(s: &str) -> Option<(i64, u32, u32, Option<u32>, Option<u32>, Option<i32>)> {
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |a: usize, z: usize| -> Option<u32> {
        let t = s.get(a..z)?;
        if t.chars().all(|c| c.is_ascii_digit()) {
            t.parse().ok()
        } else {
            None
        }
    };
    let (y, m, d) = (num(0, 4)? as i64, num(5, 7)?, num(8, 10)?);
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    if b.len() == 10 {
        return Some((y, m, d, None, None, None));
    }
    if b.len() < 16 || b[10] != b'T' || b[13] != b':' {
        return None;
    }
    let (hh, mm) = (num(11, 13)?, num(14, 16)?);
    if hh > 23 || mm > 59 {
        return None;
    }
    let rest = &s[16..];
    let off = match rest {
        "" => None,
        "Z" => Some(0),
        _ => {
            let rb = rest.as_bytes();
            if rb.len() != 6 || rb[3] != b':' || !(rb[0] == b'+' || rb[0] == b'-') {
                return None;
            }
            let oh: i32 = rest.get(1..3)?.parse().ok()?;
            let om: i32 = rest.get(4..6)?.parse().ok()?;
            if oh > 23 || om > 59 {
                return None;
            }
            let v = oh * 60 + om;
            Some(if rb[0] == b'-' { -v } else { v })
        }
    };
    Some((y, m, d, Some(hh), Some(mm), off))
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

/// Days since 1970-01-01 of a civil date. Howard Hinnant's algorithm,
/// the inverse of `commit::civil_from_days`.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Minutes since the epoch of a calendar time read as if it were UTC.
fn naive_minutes(y: i64, m: u32, d: u32, hh: u32, mm: u32) -> i64 {
    days_from_civil(y, m, d) * 1440 + hh as i64 * 60 + mm as i64
}

fn civil(secs: i64) -> (i64, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = crate::commit::civil_from_days(days);
    (y, m, d, (rem / 3600) as u32, ((rem % 3600) / 60) as u32)
}

// ----- the local zone, from the platform -------------------------------

#[cfg(unix)]
mod sys {
    /// `struct tm`: the nine ints every libc shares first, then room for
    /// the fields glibc, musl and the BSD libc add after them
    /// (`tm_gmtoff`, `tm_zone`), so the buffer is never too small.
    #[repr(C, align(8))]
    #[derive(Default)]
    pub struct Tm {
        pub sec: i32,
        pub min: i32,
        pub hour: i32,
        pub mday: i32,
        pub mon: i32,
        pub year: i32,
        pub wday: i32,
        pub yday: i32,
        pub isdst: i32,
        _tail: [u64; 8],
    }
    extern "C" {
        pub fn tzset();
        pub fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
        pub fn mktime(tm: *mut Tm) -> i64;
    }
}

/// The local calendar time of an instant.
#[cfg(unix)]
fn local_now(secs: i64) -> (i64, u32, u32, u32, u32) {
    let mut tm = sys::Tm::default();
    let t: i64 = secs;
    let ok = unsafe {
        sys::tzset();
        !sys::localtime_r(&t, &mut tm).is_null()
    };
    if !ok {
        return civil(secs);
    }
    (tm.year as i64 + 1900, tm.mon as u32 + 1, tm.mday as u32, tm.hour as u32, tm.min as u32)
}

/// The local zone's offset, in minutes east of UTC, at a local calendar
/// time (daylight saving included). Falls back to no offset when the
/// platform cannot say.
#[cfg(unix)]
fn local_offset_at(y: i64, m: u32, d: u32, hh: u32, mm: u32) -> i32 {
    let mut tm = sys::Tm { sec: 0, min: mm as i32, hour: hh as i32, mday: d as i32, mon: m as i32 - 1, year: (y - 1900) as i32, isdst: -1, ..Default::default() };
    let t = unsafe {
        sys::tzset();
        sys::mktime(&mut tm)
    };
    if t == -1 {
        return 0;
    }
    ((naive_minutes(y, m, d, hh, mm) * 60 - t) / 60) as i32
}

#[cfg(windows)]
mod sys {
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct SystemTime {
        pub year: u16,
        pub month: u16,
        pub dow: u16,
        pub day: u16,
        pub hour: u16,
        pub minute: u16,
        pub second: u16,
        pub ms: u16,
    }
    #[link(name = "kernel32")]
    extern "system" {
        pub fn GetLocalTime(t: *mut SystemTime);
        pub fn TzSpecificLocalTimeToSystemTime(tz: *const core::ffi::c_void, local: *const SystemTime, utc: *mut SystemTime) -> i32;
    }
}

#[cfg(windows)]
fn local_now(secs: i64) -> (i64, u32, u32, u32, u32) {
    let mut st = sys::SystemTime::default();
    unsafe { sys::GetLocalTime(&mut st) };
    if st.year == 0 {
        return civil(secs);
    }
    (st.year as i64, st.month as u32, st.day as u32, st.hour as u32, st.minute as u32)
}

#[cfg(windows)]
fn local_offset_at(y: i64, m: u32, d: u32, hh: u32, mm: u32) -> i32 {
    let local = sys::SystemTime { year: y as u16, month: m as u16, day: d as u16, hour: hh as u16, minute: mm as u16, ..Default::default() };
    let mut utc = sys::SystemTime::default();
    let ok = unsafe { sys::TzSpecificLocalTimeToSystemTime(std::ptr::null(), &local, &mut utc) } != 0;
    if !ok || utc.year == 0 {
        return 0;
    }
    (naive_minutes(y, m, d, hh, mm) - naive_minutes(utc.year as i64, utc.month as u32, utc.day as u32, utc.hour as u32, utc.minute as u32)) as i32
}

#[cfg(not(any(unix, windows)))]
fn local_now(secs: i64) -> (i64, u32, u32, u32, u32) {
    civil(secs)
}

#[cfg(not(any(unix, windows)))]
fn local_offset_at(_y: i64, _m: u32, _d: u32, _hh: u32, _mm: u32) -> i32 {
    0
}
