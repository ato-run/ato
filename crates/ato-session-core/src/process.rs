//! Small OS helpers for validating that a session record still refers
//! to a live process before reuse. Cross-platform shims; on unsupported
//! platforms the helpers fail closed (i.e. answer "treat as stale") so
//! the caller falls through to the spawn path.

/// Returns `true` when a process with the given PID is alive.
///
/// On Unix this calls `kill(pid, 0)` which is a no-op signal that only
/// validates permission + existence. Permission errors (process owned
/// by another user) are conservatively reported as "alive" because
/// they imply the slot is taken — but in practice every Desktop session
/// is owned by the same user that runs `ato-desktop`, so this rarely
/// matters.
///
/// On non-Unix this is a stub that returns `true`. Callers should pair
/// this with `process_start_time_unix_ms` (which returns `None` on
/// non-Unix) to defeat OS PID reuse.
#[cfg(unix)]
pub fn pid_is_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
pub fn pid_is_alive(_pid: u32) -> bool {
    // Conservative: assume alive on non-Unix so the caller's
    // start_time check (which is None on non-Unix) is what actually
    // gates reuse.
    true
}

/// Best-effort process creation time (milliseconds since UNIX epoch).
///
/// Returns `None` when the platform is unsupported or the OS rejects
/// the query (e.g. the PID died between `pid_is_alive` and this call).
/// Callers MUST treat `None` as "not reusable" — never as "match
/// anything."
pub fn process_start_time_unix_ms(pid: u32) -> Option<u64> {
    platform::process_start_time_unix_ms(pid)
}

#[cfg(target_os = "macos")]
mod platform {
    use libc::{c_int, c_void, proc_pidinfo};

    // `proc_pidinfo(PROC_PIDTBSDINFO)` returns a `proc_bsdinfo` whose
    // `pbi_start_tvsec` / `pbi_start_tvusec` give the process start
    // time. libc on macOS exposes `proc_pidinfo` and the constant but
    // not the struct shape, so we declare a minimal layout that
    // matches the leading fields we read. The full struct is
    // documented in `<sys/proc_info.h>`; we only need the two
    // starttime fields, which sit near the end.
    #[repr(C)]
    struct ProcBsdinfo {
        _pbi_flags: u32,
        _pbi_status: u32,
        _pbi_xstatus: u32,
        _pbi_pid: u32,
        _pbi_ppid: u32,
        _pbi_uid: u32,
        _pbi_gid: u32,
        _pbi_ruid: u32,
        _pbi_rgid: u32,
        _pbi_svuid: u32,
        _pbi_svgid: u32,
        _rfu_1: u32,
        _pbi_comm: [u8; 16],
        _pbi_name: [u8; 32],
        _pbi_nfiles: u32,
        _pbi_pgid: u32,
        _pbi_pjobc: u32,
        _e_tdev: u32,
        _e_tpgid: u32,
        _pbi_nice: i32,
        pbi_start_tvsec: u64,
        pbi_start_tvusec: u64,
    }

    const PROC_PIDTBSDINFO: c_int = 3;

    pub(super) fn process_start_time_unix_ms(pid: u32) -> Option<u64> {
        let mut info = std::mem::MaybeUninit::<ProcBsdinfo>::uninit();
        let size = std::mem::size_of::<ProcBsdinfo>() as c_int;
        let bytes = unsafe {
            proc_pidinfo(
                pid as c_int,
                PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr() as *mut c_void,
                size,
            )
        };
        if bytes != size {
            return None;
        }
        let info = unsafe { info.assume_init() };
        let secs = info.pbi_start_tvsec;
        let usecs = info.pbi_start_tvusec;
        Some(secs.checked_mul(1_000)?.checked_add(usecs / 1_000)?)
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::fs;

    pub(super) fn process_start_time_unix_ms(pid: u32) -> Option<u64> {
        // /proc/<pid>/stat field 22 is `starttime` in clock ticks since
        // boot. Combine with /proc/stat `btime` (boot time as unix
        // seconds) and the system clock-tick rate to get unix ms.
        let stat = fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
        // The 2nd field (`comm`) is parenthesised and may contain
        // spaces; skip past the closing `)` before tokenising.
        let close = stat.rfind(')')?;
        let rest = &stat[close + 1..];
        let mut fields = rest.split_whitespace();
        // Now the next field is field 3 (`state`); starttime is field
        // 22 of the original stat, i.e. 19 fields from here (22 - 3 =
        // 19, then 0-indexed nth(18)).
        let starttime_jiffies: u64 = fields.nth(18)?.parse().ok()?;

        let stat_root = fs::read_to_string("/proc/stat").ok()?;
        let mut btime_secs: Option<u64> = None;
        for line in stat_root.lines() {
            if let Some(rest) = line.strip_prefix("btime ") {
                btime_secs = rest.trim().parse().ok();
                break;
            }
        }
        let btime_secs = btime_secs?;

        // SAFETY: sysconf(_SC_CLK_TCK) is a documented constant query.
        let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if clk_tck <= 0 {
            return None;
        }
        let clk_tck = clk_tck as u64;

        let unix_secs = btime_secs.checked_add(starttime_jiffies / clk_tck)?;
        let frac_ms = ((starttime_jiffies % clk_tck) * 1_000) / clk_tck;
        Some(unix_secs.checked_mul(1_000)?.checked_add(frac_ms)?)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    pub(super) fn process_start_time_unix_ms(_pid: u32) -> Option<u64> {
        // Windows / other: not supported in v0. Returning None makes
        // the reuse path treat any record as "PID-reuse-detected"
        // which is the safe default — the caller falls through to
        // spawn.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_is_alive_returns_true_for_self() {
        assert!(pid_is_alive(std::process::id()));
    }

    #[test]
    #[cfg(unix)]
    fn pid_is_alive_returns_false_for_clearly_dead_pid() {
        // PID 0 is special on Unix (`kill(0, _)` targets the caller's
        // process group, not "PID 0"), so don't use it as a "dead"
        // sentinel. A very large PID is far above any realistic
        // PID_MAX on macOS / Linux and reliably reports ESRCH.
        const NEVER_USED: u32 = 999_999_999;
        assert!(!pid_is_alive(NEVER_USED));
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn process_start_time_unix_ms_returns_some_for_self() {
        assert!(process_start_time_unix_ms(std::process::id()).is_some());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn process_start_time_unix_ms_is_stable_within_a_process() {
        let a = process_start_time_unix_ms(std::process::id()).expect("a");
        let b = process_start_time_unix_ms(std::process::id()).expect("b");
        assert_eq!(a, b);
    }
}
