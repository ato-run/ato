//! Small OS helpers for validating that a session record still refers
//! to a live process before reuse. Cross-platform shims; on unsupported
//! platforms the helpers fail closed (i.e. answer "treat as stale") so
//! the caller falls through to the spawn path.

#[cfg(unix)]
pub fn current_user_owns_process(pid: u32) -> bool {
    process_owner_uid(pid).is_some_and(|uid| uid == unsafe { libc::geteuid() as u32 })
}

#[cfg(not(unix))]
pub fn current_user_owns_process(_pid: u32) -> bool {
    true
}

pub fn process_owner_uid(pid: u32) -> Option<u32> {
    platform::process_owner_uid(pid)
}

/// Returns `true` when a process with the given PID is alive.
///
/// On Unix this calls `kill(pid, 0)` which is a no-op signal that only
/// validates permission + existence. Permission errors (process owned
/// by another user) are conservatively reported as "alive" because
/// they imply the slot is taken — but in practice every Desktop session
/// is owned by the same user that runs `ato-desktop`, so this rarely
/// matters.
///
#[cfg(unix)]
pub fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }
    std::io::Error::last_os_error()
        .raw_os_error()
        .is_some_and(|errno| errno != libc::ESRCH)
}

#[cfg(windows)]
pub fn pid_is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return false;
    }
    // SAFETY: OpenProcess with the query-limited right is read-only; the
    // handle is closed before returning on every path. (This used to shell
    // out to `tasklist`, which costs a subprocess per probe — sweeps run on
    // every CLI invocation, so probes must stay in-process.)
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            // Access denied means the process exists but is not ours —
            // alive, mirroring the unix EPERM case. Any other failure
            // (notably ERROR_INVALID_PARAMETER for unknown pids) is dead.
            return std::io::Error::last_os_error().raw_os_error()
                == Some(ERROR_ACCESS_DENIED as i32);
        }
        let mut exit_code: u32 = 0;
        // A queryable handle can outlive process exit (something else may
        // hold a handle to the object), so liveness is the exit code still
        // reading STILL_ACTIVE — with the standard caveat that a process
        // which exited with code 259 is indistinguishable.
        let alive =
            GetExitCodeProcess(handle, &mut exit_code) != 0 && exit_code == STILL_ACTIVE as u32;
        CloseHandle(handle);
        alive
    }
}

#[cfg(not(any(unix, windows)))]
pub fn pid_is_alive(_pid: u32) -> bool {
    false
}

/// Environment variable that opts back into the docker fallback for OCI
/// runtime probes. Setting it to `1` makes ato consult `docker` after
/// `podman` for `inspect` / `network rm` style operations.
///
/// Default behavior (env unset or any value other than `1`) is
/// **podman-only**: docker is never spawned. This avoids a hang seen in
/// the wild where the `docker` CLI is installed but the daemon is not
/// running — `docker inspect` then waits on the unix socket with no
/// internal timeout, blocking the entire `ato` startup sweep (which is
/// invoked from every CLI subcommand, including `internal preflight`
/// that the Desktop consent screen depends on).
pub const ATO_ENABLE_DOCKER_ENV: &str = "ATO_ENABLE_DOCKER";

/// Ordered list of OCI runtime binaries to try when probing container or
/// network state. Always includes `podman`; appends `docker` only when
/// the caller has opted in via [`ATO_ENABLE_DOCKER_ENV`]. See the env-var
/// docs for the failure mode this guard prevents.
pub fn oci_probe_runtimes() -> &'static [&'static str] {
    oci_probe_runtimes_for(std::env::var(ATO_ENABLE_DOCKER_ENV).ok().as_deref())
}

/// Pure helper that decides the runtime list from an env-var-value
/// string. Split out from [`oci_probe_runtimes`] so the decision is
/// testable without process-wide env-var mutation.
fn oci_probe_runtimes_for(value: Option<&str>) -> &'static [&'static str] {
    if value == Some("1") {
        &["podman", "docker"]
    } else {
        &["podman"]
    }
}

/// Returns `true` when the given OCI container ID is currently running.
///
/// Tries the runtimes returned by [`oci_probe_runtimes`] in order. Uses
/// `--format '{{.State.Running}}'` from `inspect` to avoid parsing full
/// JSON output. Falls back to `true` (preserve) if no runtime gave a
/// definitive answer — it is safer to retain a possibly-stale record
/// than to prematurely delete an active session.
///
/// `DOCKER_HOST` from the environment is inherited automatically by the
/// child process when docker probing is enabled.
pub fn oci_container_is_running(container_id: &str) -> bool {
    if container_id.is_empty() {
        return false;
    }
    for runtime in oci_probe_runtimes() {
        let result = std::process::Command::new(runtime)
            .args(["inspect", "--format", "{{.State.Running}}", container_id])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output();
        match result {
            Ok(output) if output.status.success() => {
                // Definitive answer from this runtime: "true" or "false".
                let stdout = String::from_utf8_lossy(&output.stdout);
                return stdout.trim() == "true";
            }
            // Non-zero exit: the container may not exist in *this* runtime
            // (e.g., it's in Docker not Podman), or the daemon is unavailable.
            // Do not treat as a definitive "stopped" — try the next runtime.
            Ok(_) | Err(_) => continue,
        }
    }
    // No runtime gave a definitive answer (both absent/unavailable).
    // Preserve the record conservatively to avoid false-negative deletes.
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
        pbi_uid: u32,
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

    fn read_proc_bsdinfo(pid: u32) -> Option<ProcBsdinfo> {
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
        Some(unsafe { info.assume_init() })
    }

    pub(super) fn process_start_time_unix_ms(pid: u32) -> Option<u64> {
        let info = read_proc_bsdinfo(pid)?;
        let secs = info.pbi_start_tvsec;
        let usecs = info.pbi_start_tvusec;
        secs.checked_mul(1_000)?.checked_add(usecs / 1_000)
    }

    pub(super) fn process_owner_uid(pid: u32) -> Option<u32> {
        Some(read_proc_bsdinfo(pid)?.pbi_uid)
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
        unix_secs.checked_mul(1_000)?.checked_add(frac_ms)
    }

    pub(super) fn process_owner_uid(pid: u32) -> Option<u32> {
        let status = fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
        status.lines().find_map(|line| {
            let rest = line.strip_prefix("Uid:")?;
            rest.split_whitespace().next()?.parse().ok()
        })
    }
}

#[cfg(windows)]
mod platform {
    pub(super) fn process_start_time_unix_ms(pid: u32) -> Option<u64> {
        use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
        use windows_sys::Win32::System::Threading::{
            GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        if pid == 0 {
            return None;
        }
        let empty = || FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut creation = empty();
        let mut exit = empty();
        let mut kernel = empty();
        let mut user = empty();
        // SAFETY: query-limited handle, read-only call, handle closed on
        // every path.
        let ok = unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return None;
            }
            let ok = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) != 0;
            CloseHandle(handle);
            ok
        };
        if !ok {
            return None;
        }
        // FILETIME counts 100ns ticks since 1601-01-01; rebase to the unix
        // epoch (11644473600 seconds later) and scale to milliseconds.
        const UNIX_EPOCH_OFFSET_100NS: u64 = 116_444_736_000_000_000;
        let ticks = ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64;
        ticks
            .checked_sub(UNIX_EPOCH_OFFSET_100NS)
            .map(|unix_100ns| unix_100ns / 10_000)
    }

    pub(super) fn process_owner_uid(_pid: u32) -> Option<u32> {
        // Numeric uids are a unix concept; callers treat None as
        // "ownership unknown".
        None
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod platform {
    pub(super) fn process_start_time_unix_ms(_pid: u32) -> Option<u64> {
        // Unsupported platforms: returning None makes the reuse path treat
        // any record as "PID-reuse-detected", which is the safe default —
        // the caller falls through to spawn.
        None
    }

    pub(super) fn process_owner_uid(_pid: u32) -> Option<u32> {
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
    fn oci_probe_runtimes_defaults_to_podman_only() {
        // Default behavior (env unset). The docker fallback was the source
        // of an `ato` startup hang when docker CLI was installed but its
        // daemon was not — see `ATO_ENABLE_DOCKER_ENV` docs.
        assert_eq!(oci_probe_runtimes_for(None), &["podman"]);
    }

    #[test]
    fn oci_probe_runtimes_appends_docker_when_explicitly_enabled() {
        assert_eq!(oci_probe_runtimes_for(Some("1")), &["podman", "docker"]);
    }

    #[test]
    fn oci_probe_runtimes_treats_other_values_as_disabled() {
        // Only the literal "1" opts in. Any other value — including
        // truthy-looking strings — stays podman-only. Aligns with the
        // existing `CAPSULE_ALLOW_UNSAFE=1` / `ATO_LEGACY_SUPERVISOR=1`
        // pattern in this codebase.
        assert_eq!(oci_probe_runtimes_for(Some("0")), &["podman"]);
        assert_eq!(oci_probe_runtimes_for(Some("true")), &["podman"]);
        assert_eq!(oci_probe_runtimes_for(Some("yes")), &["podman"]);
        assert_eq!(oci_probe_runtimes_for(Some("")), &["podman"]);
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
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    fn process_start_time_unix_ms_returns_some_for_self() {
        assert!(process_start_time_unix_ms(std::process::id()).is_some());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    fn process_start_time_unix_ms_is_stable_within_a_process() {
        let a = process_start_time_unix_ms(std::process::id()).expect("a");
        let b = process_start_time_unix_ms(std::process::id()).expect("b");
        assert_eq!(a, b);
    }

    #[test]
    fn pid_is_alive_returns_true_for_long_running_child() {
        // `ping -n 60` / stdin-blocked `cat`: alive long enough to probe,
        // with no stdin dependence on Windows (pipe inheritance is fragile
        // under parallel test spawn load).
        let mut child = if cfg!(windows) {
            std::process::Command::new("ping")
                .args(["-n", "60", "127.0.0.1"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn long-running child")
        } else {
            std::process::Command::new("cat")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("spawn long-running child")
        };
        let pid = child.id();
        let alive = pid_is_alive(pid);
        child.kill().ok();
        child.wait().ok();
        assert!(alive, "long-running child pid {pid} must read alive");
    }

    #[test]
    fn pid_is_alive_returns_false_for_exited_child() {
        let mut child = if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/c", "exit", "0"])
                .spawn()
                .expect("spawn child")
        } else {
            std::process::Command::new("sh")
                .args(["-c", "exit 0"])
                .spawn()
                .expect("spawn child")
        };
        let pid = child.id();
        child.wait().expect("child exits");
        assert!(!pid_is_alive(pid), "exited child pid {pid} must read dead");
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn current_user_owns_self_process() {
        assert!(current_user_owns_process(std::process::id()));
    }
}
