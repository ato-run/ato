//! Versioned seccomp-BPF profile for the host-boundary shim.
//!
//! The workload already runs as an unprivileged uid under Landlock. This adds
//! the syscalls Landlock does not cover: creating namespaces (which would
//! escape the pod boundary), mounting, ptrace, kernel modules, eBPF, io_uring
//! and the other host-control calls. Everything else is allowed, because the
//! profile is a DENY-list: Python, PyTorch and the NVIDIA driver use a large
//! and version-dependent set of syscalls, and an allow-list guessed without
//! measuring a CUDA run would break them.
//!
//! `clone3` is answered with `ENOSYS` so glibc falls back to `clone`; `clone`
//! is allowed unless its flags request a new namespace.

use anyhow::{Context, Result, ensure};

/// Bump only when the denied set changes. It is reported by the Runner
/// capability and the Run's execution evidence.
pub const PROFILE_VERSION: &str = "host-boundary-seccomp-v1";

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH_AARCH64: u32 = 0xC000_00B7;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH_NATIVE: u32 = AUDIT_ARCH_X86_64;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH_NATIVE: u32 = AUDIT_ARCH_AARCH64;

// The namespace flags in `struct seccomp_data`'s first `clone` argument. A
// failed namespace creation must not fall back to sharing the pod's.
const CLONE_NEW_MASK: u32 = (libc::CLONE_NEWNS
    | libc::CLONE_NEWUTS
    | libc::CLONE_NEWIPC
    | libc::CLONE_NEWUSER
    | libc::CLONE_NEWPID
    | libc::CLONE_NEWNET
    | libc::CLONE_NEWCGROUP) as u32;

const RET_ALLOW: u32 = libc::SECCOMP_RET_ALLOW;
const RET_ERRNO: u32 = libc::SECCOMP_RET_ERRNO;
const RET_KILL_PROCESS: u32 = libc::SECCOMP_RET_KILL_PROCESS;

// linux/prctl.h; libc exposes the setter but not the getter.
const PR_GET_NO_NEW_PRIVS: libc::c_int = 0x27;

// Offsets in `struct seccomp_data`: nr, arch, instruction_pointer, args[0].
const OFF_NR: u32 = 0;
const OFF_ARCH: u32 = 4;
const OFF_ARG0_LO: u32 = 16;

/// Human names of the denied syscalls, for diagnostics and tests.
pub const DENIED: &[&str] = &[
    "mount",
    "umount2",
    "pivot_root",
    "chroot",
    "unshare",
    "setns",
    "ptrace",
    "process_vm_readv",
    "process_vm_writev",
    "kcmp",
    "kexec_load",
    "kexec_file_load",
    "init_module",
    "finit_module",
    "delete_module",
    "bpf",
    "userfaultfd",
    "keyctl",
    "add_key",
    "request_key",
    "open_by_handle_at",
    "name_to_handle_at",
    "reboot",
    "swapon",
    "swapoff",
    "acct",
    "settimeofday",
    "clock_settime",
    "sethostname",
    "setdomainname",
    "syslog",
    "perf_event_open",
    "lookup_dcookie",
    "fanotify_init",
    "fsopen",
    "fsconfig",
    "fsmount",
    "fspick",
    "move_mount",
    "open_tree",
    "mount_setattr",
    "pidfd_getfd",
    "process_madvise",
    "io_uring_setup",
    "io_uring_enter",
    "io_uring_register",
];

#[cfg(target_arch = "x86_64")]
const DENIED_NUMBERS: &[libc::c_long] = &[
    libc::SYS_mount,
    libc::SYS_umount2,
    libc::SYS_pivot_root,
    libc::SYS_chroot,
    libc::SYS_unshare,
    libc::SYS_setns,
    libc::SYS_ptrace,
    libc::SYS_process_vm_readv,
    libc::SYS_process_vm_writev,
    libc::SYS_kcmp,
    libc::SYS_kexec_load,
    libc::SYS_kexec_file_load,
    libc::SYS_init_module,
    libc::SYS_finit_module,
    libc::SYS_delete_module,
    libc::SYS_bpf,
    libc::SYS_userfaultfd,
    libc::SYS_keyctl,
    libc::SYS_add_key,
    libc::SYS_request_key,
    libc::SYS_open_by_handle_at,
    libc::SYS_name_to_handle_at,
    libc::SYS_reboot,
    libc::SYS_swapon,
    libc::SYS_swapoff,
    libc::SYS_acct,
    libc::SYS_settimeofday,
    libc::SYS_clock_settime,
    libc::SYS_sethostname,
    libc::SYS_setdomainname,
    libc::SYS_syslog,
    libc::SYS_perf_event_open,
    libc::SYS_lookup_dcookie,
    libc::SYS_fanotify_init,
    libc::SYS_fsopen,
    libc::SYS_fsconfig,
    libc::SYS_fsmount,
    libc::SYS_fspick,
    libc::SYS_move_mount,
    libc::SYS_open_tree,
    libc::SYS_mount_setattr,
    libc::SYS_pidfd_getfd,
    libc::SYS_process_madvise,
    libc::SYS_io_uring_setup,
    libc::SYS_io_uring_enter,
    libc::SYS_io_uring_register,
];

#[cfg(target_arch = "aarch64")]
const DENIED_NUMBERS: &[libc::c_long] = &[
    libc::SYS_mount,
    libc::SYS_umount2,
    libc::SYS_pivot_root,
    libc::SYS_chroot,
    libc::SYS_unshare,
    libc::SYS_setns,
    libc::SYS_ptrace,
    libc::SYS_process_vm_readv,
    libc::SYS_process_vm_writev,
    libc::SYS_kcmp,
    libc::SYS_kexec_load,
    libc::SYS_kexec_file_load,
    libc::SYS_init_module,
    libc::SYS_finit_module,
    libc::SYS_delete_module,
    libc::SYS_bpf,
    libc::SYS_userfaultfd,
    libc::SYS_keyctl,
    libc::SYS_add_key,
    libc::SYS_request_key,
    libc::SYS_open_by_handle_at,
    libc::SYS_name_to_handle_at,
    libc::SYS_reboot,
    libc::SYS_swapon,
    libc::SYS_swapoff,
    libc::SYS_acct,
    libc::SYS_settimeofday,
    libc::SYS_clock_settime,
    libc::SYS_sethostname,
    libc::SYS_setdomainname,
    libc::SYS_syslog,
    libc::SYS_perf_event_open,
    libc::SYS_lookup_dcookie,
    libc::SYS_fanotify_init,
    libc::SYS_fsopen,
    libc::SYS_fsconfig,
    libc::SYS_fsmount,
    libc::SYS_fspick,
    libc::SYS_move_mount,
    libc::SYS_open_tree,
    libc::SYS_mount_setattr,
    libc::SYS_pidfd_getfd,
    libc::SYS_process_madvise,
    libc::SYS_io_uring_setup,
    libc::SYS_io_uring_enter,
    libc::SYS_io_uring_register,
];

const _: () = assert!(DENIED.len() == DENIED_NUMBERS.len());

fn stmt(code: u32, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code: code as u16,
        jt: 0,
        jf: 0,
        k,
    }
}

fn jump(code: u32, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter {
        code: code as u16,
        jt,
        jf,
        k,
    }
}

fn program() -> Vec<libc::sock_filter> {
    let mut filter = Vec::with_capacity(8 + DENIED_NUMBERS.len() * 2);
    // A filter built for another ABI must not be evaluated with these numbers.
    filter.push(stmt(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, OFF_ARCH));
    filter.push(jump(
        libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
        AUDIT_ARCH_NATIVE,
        1,
        0,
    ));
    filter.push(stmt(libc::BPF_RET | libc::BPF_K, RET_KILL_PROCESS));

    filter.push(stmt(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, OFF_NR));

    // clone3 carries no inspectable flags: answer ENOSYS so glibc uses clone.
    filter.push(jump(
        libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
        libc::SYS_clone3 as u32,
        0,
        1,
    ));
    filter.push(stmt(
        libc::BPF_RET | libc::BPF_K,
        RET_ERRNO | libc::ENOSYS as u32,
    ));

    // clone is allowed for threads, denied when it asks for a namespace.
    filter.push(jump(
        libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
        libc::SYS_clone as u32,
        0,
        4,
    ));
    filter.push(stmt(
        libc::BPF_LD | libc::BPF_W | libc::BPF_ABS,
        OFF_ARG0_LO,
    ));
    filter.push(stmt(
        libc::BPF_ALU | libc::BPF_AND | libc::BPF_K,
        CLONE_NEW_MASK,
    ));
    filter.push(jump(libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K, 0, 1, 0));
    filter.push(stmt(
        libc::BPF_RET | libc::BPF_K,
        RET_ERRNO | libc::EPERM as u32,
    ));

    for number in DENIED_NUMBERS {
        filter.push(jump(
            libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
            *number as u32,
            0,
            1,
        ));
        filter.push(stmt(
            libc::BPF_RET | libc::BPF_K,
            RET_ERRNO | libc::EPERM as u32,
        ));
    }
    filter.push(stmt(libc::BPF_RET | libc::BPF_K, RET_ALLOW));
    filter
}

/// Whether this kernel exposes seccomp filter mode with `SECCOMP_RET_ERRNO`.
///
/// A kernel that cannot execute the profile must refuse the launch rather than
/// run the workload without it.
pub fn is_supported() -> bool {
    std::fs::read_to_string("/proc/sys/kernel/seccomp/actions_avail")
        .is_ok_and(|actions| actions.split_whitespace().any(|action| action == "errno"))
}

/// Apply the profile to the current thread. Inherited across `exec` and by
/// every child, so the workload cannot drop it.
///
/// Requires `PR_SET_NO_NEW_PRIVS` first: without it the kernel refuses a filter
/// from a process that could otherwise gain privilege.
pub fn apply() -> Result<()> {
    let nnp = unsafe { libc::prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) };
    ensure!(
        nnp == 1,
        "seccomp requires PR_SET_NO_NEW_PRIVS before the filter is applied"
    );
    let mut filter = program();
    let mut prog = libc::sock_fprog {
        len: filter
            .len()
            .try_into()
            .context("seccomp filter is too long for sock_fprog")?,
        filter: filter.as_mut_ptr(),
    };
    // SAFETY: the kernel reads `prog` (a length plus a pointer to `prog.len`
    // scalar instructions that outlive this call) and installs a filter. No
    // caller memory is written, and failure is reported through the result.
    let result = unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER as libc::c_ulong,
            &mut prog as *mut libc::sock_fprog as libc::c_ulong,
            0,
            0,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to install seccomp filter");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_profile_has_one_number_for_every_named_syscall() {
        assert_eq!(PROFILE_VERSION, "host-boundary-seccomp-v1");
        assert_eq!(DENIED.len(), DENIED_NUMBERS.len());
        for name in ["unshare", "setns", "mount", "ptrace", "bpf", "fsopen"] {
            assert!(DENIED.contains(&name), "{name} is not denied");
        }
    }

    #[test]
    fn the_program_ends_in_allow_and_starts_with_an_arch_check() {
        let filter = program();
        assert_eq!(filter.first().unwrap().k, OFF_ARCH);
        assert_eq!(filter.last().unwrap().k, RET_ALLOW);
        assert_eq!(filter[2].k, RET_KILL_PROCESS);
    }

    #[test]
    fn support_check_does_not_panic() {
        let _ = is_supported();
    }
}
