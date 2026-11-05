#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub free: u64,
    pub used: u64,
}

pub fn usage(path: &str) -> Option<Usage> {
    let c_path = std::ffi::CString::new(path).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    let block = stat.f_frsize as u64;
    Some(Usage {
        free: stat.f_bavail as u64 * block,
        used: (stat.f_blocks as u64 - stat.f_bfree as u64) * block,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_real_path_reports_a_plausible_volume() {
        let usage = super::usage(".").expect("the working directory is on a mounted volume");
        assert!(usage.free > 0, "{usage:?}");
        assert!(usage.used > 0, "{usage:?}");
        assert!(usage.free + usage.used > 1 << 30, "under a gigabyte total: {usage:?}");
    }

    #[test]
    fn a_path_that_is_not_there_is_not_zero_free() {
        assert_eq!(super::usage("/no/such/mount/point"), None);
    }
}
