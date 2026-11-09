//! Windows OS primitives a native desktop Adapter needs.
//!
//! These live with the other OS primitives (and not in the portable
//! application crate, which forbids `unsafe` crate-wide) because each one is a
//! raw Win32 call:
//!
//! * Authenticode verification through `WinVerifyTrust`, producing one
//!   recorded verdict;
//! * Job Object ownership with `KILL_ON_JOB_CLOSE`, plus an explicit
//!   terminate, which is the process-tree stop primitive;
//! * an AppContainer policy probe, which currently reports that the policy
//!   cannot be constructed rather than pretending it can.
//!
//! None of this claims containment. The Adapter that owns admission decides
//! what may run; this module only reports what the OS can do.

#![cfg(windows)]

use std::path::Path;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::WinTrust::{
    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
    WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
    WinVerifyTrust,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};
use windows::core::PCWSTR;

/// One Authenticode observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthenticodeReport {
    /// `true` when `WinVerifyTrust` returned success.
    pub valid: bool,
    /// The raw trust status. Zero is success; any other value is the reason.
    pub status: i32,
}

/// Whether this host can produce an Authenticode verdict at all.
///
/// `WinVerifyTrust` is part of the OS and is always linked on Windows; the
/// function exists so a caller records a measured fact rather than assuming.
pub fn authenticode_verification_available() -> bool {
    true
}

fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Verify one file's Authenticode signature.
pub fn verify_authenticode(path: &Path) -> AuthenticodeReport {
    let wide_path = wide(path);
    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(wide_path.as_ptr()),
        hFile: HANDLE::default(),
        pgKnownSubject: std::ptr::null_mut(),
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 {
            pFile: &mut file_info,
        },
        dwStateAction: WTD_STATEACTION_VERIFY,
        ..Default::default()
    };
    // SAFETY: every pointer refers to a local value that outlives both calls,
    // and no `pSignatureSettings` (which would own memory) is supplied.
    let status = unsafe {
        WinVerifyTrust(
            windows::Win32::Foundation::HWND::default(),
            &mut action,
            &mut data as *mut WINTRUST_DATA as *mut core::ffi::c_void,
        )
    };
    // Release whatever WinVerifyTrust allocated for the verify call.
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: same buffers; the close action only frees the provider state.
    unsafe {
        WinVerifyTrust(
            windows::Win32::Foundation::HWND::default(),
            &mut action,
            &mut data as *mut WINTRUST_DATA as *mut core::ffi::c_void,
        );
    }
    AuthenticodeReport {
        valid: status == 0,
        status,
    }
}

/// A Job Object that kills every member when the last handle closes.
///
/// This is the process-tree ownership primitive: the workload and every child
/// it creates are assigned to the job, so a stop terminates the tree even when
/// the leader already exited.
pub struct JobObject {
    handle: HANDLE,
}

impl JobObject {
    pub fn create() -> Result<Self, String> {
        // SAFETY: no security attributes and no name are requested; the
        // returned handle is owned by this value.
        let handle = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
            .map_err(|error| format!("CreateJobObjectW failed: {error}"))?;
        // SAFETY: the struct is zeroed and then fully populated for the size
        // that is passed; this is the documented representation.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `info` outlives the call and its size matches.
        unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .map_err(|error| format!("SetInformationJobObject failed: {error}"))?;
        Ok(Self { handle })
    }

    /// Put one started process (by pid) into this job.
    pub fn assign(&self, pid: u32) -> Result<(), String> {
        // SAFETY: opening the process asks only for the rights needed to place
        // it in the job; the handle is closed before returning.
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid) }
            .map_err(|error| format!("OpenProcess({pid}) failed: {error}"))?;
        // SAFETY: both handles are live here.
        let assigned = unsafe { AssignProcessToJobObject(self.handle, process) };
        // SAFETY: `process` is the handle opened above and is not used again.
        let _ = unsafe { CloseHandle(process) };
        assigned.map_err(|error| format!("AssignProcessToJobObject({pid}) failed: {error}"))
    }

    /// Terminate every process in this job.
    pub fn terminate(&self) -> Result<(), String> {
        // SAFETY: the job handle is live for as long as `self` is.
        unsafe { TerminateJobObject(self.handle, 1) }
            .map_err(|error| format!("TerminateJobObject failed: {error}"))
    }
}

impl Drop for JobObject {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            // SAFETY: this value owns the only handle and Drop runs once.
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }
}

/// Attempt to construct the AppContainer policy the mandatory set needs.
///
/// Scoping exactly the Run's workspace, one writable state directory and the
/// declared inbound loopback Ports needs an AppContainer profile plus
/// capability SIDs this build does not create. Reporting the gap is what keeps
/// the Adapter fail-closed instead of claiming containment it does not have.
pub fn appcontainer_policy_available() -> Result<(), String> {
    Err(
        "an AppContainer profile scoping exactly the Run's workspace, writable state directory \
         and inbound loopback Ports is not implemented in this build"
            .to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_object_can_be_created_and_terminated() {
        let job = JobObject::create().expect("a Job Object can be created");
        job.terminate().expect("an empty job terminates");
    }

    #[test]
    fn an_appcontainer_policy_is_reported_as_unavailable() {
        assert!(appcontainer_policy_available().is_err());
        assert!(authenticode_verification_available());
    }

    #[test]
    fn authenticode_returns_a_verdict_for_a_system_binary() {
        let report = verify_authenticode(Path::new(r"C:\Windows\System32\notepad.exe"));
        // The verdict is recorded; this test does not assume either outcome.
        let _ = report.valid;
    }
}
