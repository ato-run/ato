//! Live seccomp checks. Linux only; the profile is installed by the real shim
//! path and the dangerous calls are attempted in the filtered process.
#![cfg(target_os = "linux")]

use std::process::Command;

const CHILD: &str = "ATO_SECCOMP_PROBE_CHILD";

fn errno_is_eperm(result: libc::c_int) -> bool {
    result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Re-run this one test in a fresh process, so the filter never lands on the
/// test harness that started it.
#[test]
fn the_profile_denies_escape_syscalls_and_keeps_python_style_workloads_working() {
    if std::env::var_os(CHILD).is_some() {
        probe();
        return;
    }
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "the_profile_denies_escape_syscalls_and_keeps_python_style_workloads_working",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .status()
        .expect("re-run the seccomp probe");
    assert!(status.success(), "the filtered probe process failed");
}

fn probe() {
    // SAFETY: the probe process is a throwaway; every call below is a scalar
    // syscall whose result is checked.
    unsafe {
        assert_eq!(
            libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0),
            0,
            "no_new_privs must be set before seccomp"
        );
    }
    ato_sandbox::apply_seccomp().expect("the seccomp profile must install");

    // Creating any namespace is refused, including user namespaces.
    // SAFETY: scalar syscall; the result is the assertion.
    let unshare = unsafe { libc::unshare(libc::CLONE_NEWUSER) };
    assert!(errno_is_eperm(unshare), "unshare was not denied");
    // SAFETY: scalar syscall; the result is the assertion.
    let mount = unsafe {
        libc::mount(
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            std::ptr::null(),
        )
    };
    assert!(errno_is_eperm(mount), "mount was not denied");
    // SAFETY: PTRACE_TRACEME takes no address and reads no memory.
    let ptrace = unsafe { libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) };
    assert!(
        errno_is_eperm(ptrace as libc::c_int),
        "ptrace was not denied"
    );
    // clone with a namespace flag is denied even though plain clone is not.
    // SAFETY: the flags alone are requested; the result is the assertion.
    let clone = unsafe {
        libc::syscall(
            libc::SYS_clone,
            libc::CLONE_NEWNS as libc::c_ulong,
            std::ptr::null::<libc::c_void>(),
            std::ptr::null::<libc::c_void>(),
            std::ptr::null::<libc::c_void>(),
            std::ptr::null::<libc::c_void>(),
        )
    };
    assert_eq!(
        clone, -1,
        "clone with CLONE_NEWNS was not denied (returned {clone})"
    );

    // x32 shares AUDIT_ARCH_X86_64 with native x86-64. Its syscall numbers
    // must not slip through the native deny/allow decisions.
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: scalar syscall; the result is the assertion.
        let x32 = unsafe { libc::syscall(0x4000_0000u64 | libc::SYS_getpid as u64) };
        assert_eq!(x32, -1, "an x32 syscall was not refused");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ENOSYS)
        );
    }

    // A filter survives fork: a grandchild cannot escape either.
    // SAFETY: after fork the child only makes one scalar syscall and exits.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed");
    if pid == 0 {
        // SAFETY: scalar syscall and `_exit`; no allocation after fork.
        let refused = unsafe { libc::unshare(libc::CLONE_NEWUSER) };
        unsafe { libc::_exit(if errno_is_eperm(refused) { 0 } else { 1 }) };
    }
    let mut status = 0;
    // SAFETY: `pid` is our child and `status` is a valid out-pointer.
    assert!(unsafe { libc::waitpid(pid, &mut status, 0) } == pid);
    assert!(status == 0, "the forked child escaped the filter");

    // Normal runtime behaviour is untouched: threads, exec and a Python-style
    // subprocess all still work.
    std::thread::spawn(|| 1).join().expect("threads still work");
    let shell = Command::new("/bin/sh")
        .args(["-c", "printf 42"])
        .output()
        .expect("a subprocess still starts");
    assert!(
        shell.status.success(),
        "the subprocess failed under seccomp"
    );
    assert_eq!(String::from_utf8_lossy(&shell.stdout), "42");
    // The minimal test image may not carry Python; when it does, it runs.
    if Command::new("python3").arg("--version").output().is_ok() {
        let python = Command::new("python3")
            .args(["-c", "print(6 * 7)"])
            .output()
            .expect("python3 starts");
        assert!(python.status.success(), "python3 failed under seccomp");
        assert_eq!(String::from_utf8_lossy(&python.stdout).trim(), "42");
    }
}
