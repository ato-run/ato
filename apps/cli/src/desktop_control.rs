//! Hidden machine interface used by the desktop shell to inspect an active Run.
//!
//! The desktop shell never reads `.capsule/` directly. It asks this CLI-owned
//! boundary for the current Run status and the explicit loopback listen
//! addresses that are safe to present as Web surfaces.

use std::net::SocketAddr;
use std::path::Path;

use anyhow::Result;
use ato_adapter_http::HTTP_ADAPTER_ID;
use ato_ipc::desktop_control::{DesktopRunStatus, DesktopRunView, DesktopSurfaceView};
use ato_ipc::session_surface::WEB_SURFACE_PROFILE;
use ato_objects::{ActiveRun, LocalCapsuleRepository};

use crate::authoring::load_runtime_state;

/// Inspects the active Run of `project` and returns a presentation-oriented
/// view. There is intentionally no active Run → surface derivation outside
/// this boundary: the surface list is derived from Runtime state only when the
/// Run is `active`.
pub(crate) fn inspect(project: &Path) -> Result<DesktopRunView> {
    let repository = LocalCapsuleRepository::open(project)?;
    let Some(run) = repository.active_run()? else {
        return Ok(DesktopRunView {
            project: project.display().to_string(),
            branch: String::new(),
            head: String::new(),
            status: DesktopRunStatus::Inactive,
            surfaces: Vec::new(),
        });
    };
    let status = status_of(&run);
    let surfaces = if status == DesktopRunStatus::Active {
        web_surfaces(&repository, &run)?
    } else {
        Vec::new()
    };
    Ok(DesktopRunView {
        project: project.display().to_string(),
        branch: run.branch,
        head: run.head.to_string(),
        status,
        surfaces,
    })
}

fn status_of(run: &ActiveRun) -> DesktopRunStatus {
    match run.status.as_str() {
        "starting" => DesktopRunStatus::Starting,
        "active" => DesktopRunStatus::Active,
        _ => DesktopRunStatus::Failed,
    }
}

/// Derives Web surfaces from the Runtime state of the active Run, keeping only
/// explicit loopback listen addresses. `0.0.0.0`, non-loopback addresses, and
/// non-HTTP adapters are rejected for the MVP.
fn web_surfaces(
    repository: &LocalCapsuleRepository,
    run: &ActiveRun,
) -> Result<Vec<DesktopSurfaceView>> {
    let state = load_runtime_state(&run.head, repository.objects())?;
    Ok(state
        .config
        .adapter
        .iter()
        .filter(|adapter| adapter.use_adapter == HTTP_ADAPTER_ID)
        .filter_map(|adapter| adapter.listen.as_deref().and_then(loopback_web_surface))
        .collect())
}

/// Maps a configured listen string to a Web surface only when it is an
/// explicit, numeric loopback address. `0.0.0.0`, remote hosts, hostnames, and
/// unparsable addresses are refused.
fn loopback_web_surface(listen: &str) -> Option<DesktopSurfaceView> {
    let address = listen.parse::<SocketAddr>().ok()?;
    if !address.ip().is_loopback() {
        return None;
    }
    Some(DesktopSurfaceView::Web {
        url: format!("http://{address}"),
        profile: WEB_SURFACE_PROFILE.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use ato_computation::ComputationRef;

    use super::*;

    fn reference(byte: char) -> ComputationRef {
        ComputationRef::parse(format!("blake3:{}", byte.to_string().repeat(64))).unwrap()
    }

    #[test]
    fn status_maps_known_lease_states() {
        fn run(status: &str) -> ActiveRun {
            ActiveRun {
                token: "t".to_owned(),
                branch: "main".to_owned(),
                branch_base: reference('a'),
                head: reference('a'),
                record_seq: 0,
                pid: 1,
                process_start_time: String::new(),
                process_group: 1,
                boot_session: String::new(),
                status: status.to_owned(),
            }
        }
        assert_eq!(status_of(&run("starting")), DesktopRunStatus::Starting);
        assert_eq!(status_of(&run("active")), DesktopRunStatus::Active);
        assert_eq!(status_of(&run("unexpected")), DesktopRunStatus::Failed);
    }

    #[test]
    fn explicit_loopback_is_a_web_surface() {
        let surface = loopback_web_surface("127.0.0.1:8000").unwrap();
        assert_eq!(
            surface,
            DesktopSurfaceView::Web {
                url: "http://127.0.0.1:8000".to_owned(),
                profile: WEB_SURFACE_PROFILE.to_owned(),
            }
        );
    }

    #[test]
    fn non_loopback_and_hostname_listens_are_refused() {
        assert!(loopback_web_surface("0.0.0.0:8000").is_none());
        assert!(loopback_web_surface("192.168.1.5:8000").is_none());
        assert!(loopback_web_surface("localhost:8000").is_none());
        assert!(loopback_web_surface("example.com:8000").is_none());
        assert!(loopback_web_surface("not-an-address").is_none());
    }
}
