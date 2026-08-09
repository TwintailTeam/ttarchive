pub const DOS_EPOCH: i64 = 315_532_800;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DosDateTime {
    pub time: u16,
    pub date: u16,
}

impl DosDateTime {
    pub const MIN: DosDateTime = DosDateTime { time: 0, date: 0x0021 };
}

fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
}

pub fn civil_from_unix(secs: i64) -> Civil {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    Civil { year, month, day, hour: (rem / 3600) as u32, minute: ((rem % 3600) / 60) as u32, second: (rem % 60) as u32 }
}

pub fn unix_from_civil(c: Civil) -> i64 {
    days_from_civil(c.year, c.month, c.day) * 86_400 + c.hour as i64 * 3600 + c.minute as i64 * 60 + c.second as i64
}

pub fn to_dos(unix_secs: i64) -> DosDateTime {
    if unix_secs < DOS_EPOCH {
        return DosDateTime::MIN;
    }

    let c = civil_from_unix(unix_secs);
    if c.year > 2107 {
        return DosDateTime { time: 0xBF7D, date: 0xFF9F };
    }

    let time = ((c.hour as u16) << 11) | ((c.minute as u16) << 5) | (c.second as u16 / 2);
    let date = (((c.year - 1980) as u16) << 9) | ((c.month as u16) << 5) | (c.day as u16);
    DosDateTime { time, date }
}

pub fn from_dos(dt: DosDateTime) -> i64 {
    let c = Civil {
        year: 1980 + (dt.date >> 9) as i32,
        month: ((dt.date >> 5) & 0x0f).clamp(1, 12) as u32,
        day: (dt.date & 0x1f).clamp(1, 31) as u32,
        hour: ((dt.time >> 11) & 0x1f).min(23) as u32,
        minute: ((dt.time >> 5) & 0x3f).min(59) as u32,
        second: ((dt.time & 0x1f) * 2).min(58) as u32,
    };
    unix_from_civil(c)
}

pub fn unix_from_filetime(ticks: u64) -> i64 {
    (ticks / 10_000_000) as i64 - 11_644_473_600
}

pub fn filetime_from_unix(secs: i64) -> u64 {
    let shifted = secs.saturating_add(11_644_473_600).max(0) as u64;
    shifted.saturating_mul(10_000_000)
}
