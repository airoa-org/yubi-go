use std::time::{SystemTime, UNIX_EPOCH};

pub fn boottime_ns() -> u64 {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const BOOTTIME: libc::clockid_t = libc::CLOCK_BOOTTIME;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    const BOOTTIME: libc::clockid_t = libc::CLOCK_MONOTONIC;
    clock_ns(BOOTTIME)
}

pub fn realtime_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64
}

fn clock_ns(id: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(id, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

pub fn rfc3339(realtime_ns: u64) -> String {
    let secs = (realtime_ns / 1_000_000_000) as i64;
    let millis = (realtime_ns % 1_000_000_000) / 1_000_000;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);

    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_agrees_with_the_dates_meta_json_already_carries() {
        assert_eq!(rfc3339(1_787_154_123_798_000_000), "2026-08-19T15:42:03.798Z");
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(1_709_208_000_000_000_000), "2024-02-29T12:00:00.000Z");
    }
}
