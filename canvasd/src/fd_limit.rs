//! canvasd's open-file limit. launchd starts agents with a soft
//! `RLIMIT_NOFILE` of 256, and a viewer reload opens one socket connection
//! per card image and data fetch, so the accept loop would hit EMFILE and
//! stall every client. `raise` lifts the soft limit toward the hard one.

/// The soft limit before and after `raise`, and the hard limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FdLimit {
    pub before: u64,
    pub soft: u64,
    pub hard: u64,
}

/// The cap used when `kern.maxfilesperproc` can't be read: macOS's
/// `OPEN_MAX` from `<sys/syslimits.h>`.
const OPEN_MAX: u64 = 10240;

/// Raise this process's soft `RLIMIT_NOFILE` to the hard limit, capped at
/// `kern.maxfilesperproc` (macOS refuses `RLIM_INFINITY` and anything above
/// that cap). Returns the limits, or why they couldn't be read or set.
pub fn raise() -> std::io::Result<FdLimit> {
    let current = get()?;
    let before = current.rlim_cur as u64;
    let hard = current.rlim_max as u64;
    let soft = hard.min(max_files_per_proc().unwrap_or(OPEN_MAX));
    if soft <= before {
        return Ok(FdLimit {
            before,
            soft: before,
            hard,
        });
    }
    set(soft, current.rlim_max)?;
    Ok(FdLimit { before, soft, hard })
}

fn get() -> std::io::Result<libc::rlimit> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes one rlimit through a valid pointer.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(limit)
}

fn set(soft: u64, hard: libc::rlim_t) -> std::io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: soft as libc::rlim_t,
        rlim_max: hard,
    };
    // SAFETY: setrlimit reads one rlimit through a valid pointer.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn max_files_per_proc() -> Option<u64> {
    let mut value: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is NUL-terminated and value/size describe a c_int.
    let ret = unsafe {
        libc::sysctlbyname(
            c"kern.maxfilesperproc".as_ptr(),
            (&mut value as *mut libc::c_int).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (ret == 0 && value > 0).then_some(value as u64)
}

#[cfg(not(target_os = "macos"))]
fn max_files_per_proc() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn raise_never_lowers_the_soft_limit_or_passes_the_hard_one() {
        let limit = super::raise().unwrap();
        assert!(limit.soft >= limit.before, "{limit:?}");
        assert!(limit.soft <= limit.hard, "{limit:?}");
        assert_eq!(super::get().unwrap().rlim_cur as u64, limit.soft);
    }
}
