//! Windows taskbar Jump List (KOH-41).
//!
//! Right-clicking the Ato Desktop taskbar button shows a Windows **Jump List**.
//! This module adds a "Tasks" category with the same lifecycle actions as the
//! system-tray menu:
//!
//! ```text
//! Open Ato
//! Running Apps
//! Stop All Running Apps
//! Quit Ato
//! ```
//!
//! ## Why a control pipe
//!
//! Jump List tasks are `IShellLink`s: clicking one *launches the exe* with
//! arguments (`--jump-action <token>`) — Windows does not message the running
//! process. So a task invocation forwards its action to the already-running
//! instance over a stable named pipe and exits, instead of opening a second
//! Desktop. Normal launches (no `--jump-action`) are unaffected.
//!
//! The forwarded action is drained by a GPUI foreground poll task and dispatched
//! through [`crate::window::tray::handle_action`], so the taskbar and the system
//! tray share one set of handlers.

use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};

use gpui::App;

use crate::window::tray::{ID_OPEN, ID_QUIT, ID_RUNNING, ID_STOP_ALL, handle_action};

/// Stable, instance-independent control pipe. A secondary `--jump-action`
/// invocation connects here to forward its action to the running Desktop.
const CONTROL_PIPE_PATH: &str = r"\\.\pipe\run.ato.desktop.control";

/// Actions received over the control pipe, drained on the GPUI thread.
static CONTROL_QUEUE: LazyLock<Mutex<VecDeque<String>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));

/// Install the taskbar Jump List integration: start the control-pipe listener,
/// the GPUI drain task, and register the Jump List. Failures are logged, never
/// fatal (the taskbar menu is a convenience surface).
pub fn install_taskbar(cx: &mut App) {
    spawn_control_listener();
    spawn_control_poll(cx);
    match unsafe { register_jump_list() } {
        Ok(()) => tracing::info!("taskbar: Jump List registered"),
        Err(err) => tracing::error!(?err, "taskbar: Jump List registration failed"),
    }
}

/// Forward a Jump List action token to the running Desktop over the control
/// pipe. Returns `true` if a running instance accepted it. Called from `main`
/// for `--jump-action <token>` before any window is created.
pub fn forward_jump_action(action: &str) -> bool {
    use std::io::Write;
    use std::time::Duration;

    // ERROR_PIPE_BUSY (231): a connection is mid-handshake — retry briefly.
    for _ in 0..20 {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(CONTROL_PIPE_PATH)
        {
            Ok(mut file) => {
                let _ = writeln!(file, "{action}");
                let _ = file.flush();
                return true;
            }
            Err(err) if err.raw_os_error() == Some(231) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return false,
        }
    }
    false
}

/// True if `arg` is a recognised Jump List action token.
pub fn is_known_action(arg: &str) -> bool {
    matches!(arg, ID_OPEN | ID_RUNNING | ID_STOP_ALL | ID_QUIT)
}

// ─── Control pipe listener ───────────────────────────────────────────────────

fn spawn_control_listener() {
    use std::io::{BufRead, BufReader};
    use std::os::windows::io::FromRawHandle;

    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    let name: Vec<u16> = CONTROL_PIPE_PATH
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let spawned = std::thread::Builder::new()
        .name("ato-desktop-taskbar-control".into())
        .spawn(move || {
            loop {
                // SAFETY: NUL-terminated wide name kept alive; default security
                // descriptor scopes the pipe to the current user.
                let handle = unsafe {
                    CreateNamedPipeW(
                        name.as_ptr(),
                        PIPE_ACCESS_DUPLEX,
                        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                        8,
                        4096,
                        4096,
                        0,
                        std::ptr::null_mut(),
                    )
                };
                if handle == INVALID_HANDLE_VALUE {
                    tracing::error!(
                        "taskbar: CreateNamedPipeW failed: {}",
                        std::io::Error::last_os_error()
                    );
                    break;
                }
                let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) } != 0
                    || std::io::Error::last_os_error().raw_os_error()
                        == Some(ERROR_PIPE_CONNECTED as i32);
                if !connected {
                    unsafe { CloseHandle(handle) };
                    continue;
                }
                // Take ownership of the handle so it is closed when the reader
                // drops (disconnecting the pipe instance).
                let file = unsafe { std::fs::File::from_raw_handle(handle as *mut _) };
                let mut reader = BufReader::new(file);
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() {
                    let action = line.trim().to_string();
                    if !action.is_empty()
                        && let Ok(mut queue) = CONTROL_QUEUE.lock()
                    {
                        queue.push_back(action);
                    }
                }
            }
        });
    if let Err(err) = spawned {
        tracing::error!(?err, "taskbar: failed to spawn control listener");
    }
}

fn spawn_control_poll(cx: &mut App) {
    let async_app = cx.to_async();
    async_app
        .foreground_executor()
        .spawn({
            let be = async_app.background_executor().clone();
            let aa = async_app.clone();
            async move {
                use std::time::Duration;
                loop {
                    be.timer(Duration::from_millis(150)).await;
                    loop {
                        let next = CONTROL_QUEUE.lock().ok().and_then(|mut q| q.pop_front());
                        match next {
                            Some(action) => handle_action(&aa, &action),
                            None => break,
                        }
                    }
                }
            }
        })
        .detach();
}

// ─── Jump List registration (COM) ────────────────────────────────────────────

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn register_jump_list() -> windows::core::Result<()> {
    use windows::Win32::Foundation::{E_OUTOFMEMORY, PROPERTYKEY};
    use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemAlloc,
    };
    use windows::Win32::System::Variant::VT_LPWSTR;
    use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{ICustomDestinationList, IShellLinkW};
    use windows::core::{GUID, Interface, PCWSTR, PWSTR};

    // CLSIDs (not all exposed as named constants across windows-rs versions).
    let clsid_destination_list = GUID::from_u128(0x77f10cf0_3db5_4966_b520_b7c54fd35ed6);
    let clsid_enumerable_collection = GUID::from_u128(0x2d3468c1_36a7_43b6_ac24_d3f02fd9607a);
    let clsid_shell_link = GUID::from_u128(0x00021401_0000_0000_c000_000000000046);
    // PKEY_Title = {F29F85E0-4FF9-1068-AB91-08002B27B3D9}, 2
    let pkey_title = PROPERTYKEY {
        fmtid: GUID::from_u128(0xf29f85e0_4ff9_1068_ab91_08002b27b3d9),
        pid: 2,
    };

    // GPUI initialises OLE on the main thread; re-init returns S_FALSE. Do not
    // uninitialise — we are sharing the thread's apartment.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    let exe = std::env::current_exe()
        .map(|p| to_wide(&p.to_string_lossy()))
        .unwrap_or_default();

    unsafe {
        let dest_list: ICustomDestinationList =
            CoCreateInstance(&clsid_destination_list, None, CLSCTX_INPROC_SERVER)?;
        let mut min_slots: u32 = 0;
        // Required handshake; the returned array is the user-removed items.
        let _removed: IObjectArray = dest_list.BeginList(&mut min_slots)?;

        let collection: IObjectCollection =
            CoCreateInstance(&clsid_enumerable_collection, None, CLSCTX_INPROC_SERVER)?;

        for (action, title) in [
            (ID_OPEN, "Open Ato"),
            (ID_RUNNING, "Running Apps"),
            (ID_STOP_ALL, "Stop All Running Apps"),
            (ID_QUIT, "Quit Ato"),
        ] {
            let link: IShellLinkW =
                CoCreateInstance(&clsid_shell_link, None, CLSCTX_INPROC_SERVER)?;
            if !exe.is_empty() {
                link.SetPath(PCWSTR(exe.as_ptr()))?;
                link.SetIconLocation(PCWSTR(exe.as_ptr()), 0)?;
            }
            let args = to_wide(&format!("--jump-action {action}"));
            link.SetArguments(PCWSTR(args.as_ptr()))?;

            // The visible task label comes from PKEY_Title on the link's
            // property store (SetDescription only sets the tooltip). The
            // VT_LPWSTR string MUST be CoTaskMem-allocated: SetValue copies it,
            // and PropVariantClear frees our copy via CoTaskMemFree — handing the
            // store a Rust-allocated pointer corrupts the heap.
            let store: IPropertyStore = link.cast()?;
            let title_w = to_wide(title);
            let mem = CoTaskMemAlloc(title_w.len() * std::mem::size_of::<u16>()) as *mut u16;
            if mem.is_null() {
                return Err(windows::core::Error::from_hresult(E_OUTOFMEMORY));
            }
            std::ptr::copy_nonoverlapping(title_w.as_ptr(), mem, title_w.len());
            let mut value = PROPVARIANT::default();
            {
                let slot = &mut value.Anonymous.Anonymous;
                slot.vt = VT_LPWSTR;
                slot.Anonymous.pwszVal = PWSTR(mem);
            }
            let set = store.SetValue(&pkey_title, &value);
            let _ = PropVariantClear(&mut value);
            set?;
            store.Commit()?;

            collection.AddObject(&link)?;
        }

        let tasks: IObjectArray = collection.cast()?;
        dest_list.AddUserTasks(&tasks)?;
        dest_list.CommitList()?;
    }
    Ok(())
}
