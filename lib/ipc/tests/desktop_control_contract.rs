//! Public wire-contract tests for the desktop Run inspection DTOs.

use ato_ipc::desktop_control::{
    DesktopRunInspectRequest, DesktopRunStatus, DesktopRunView, DesktopSurfaceView,
};
use ato_ipc::session_surface::WEB_SURFACE_PROFILE;

#[test]
fn inactive_run_view_round_trips() {
    let view = DesktopRunView {
        project: "demo".to_owned(),
        branch: String::new(),
        head: String::new(),
        status: DesktopRunStatus::Inactive,
        surfaces: Vec::new(),
    };
    let bytes = serde_json::to_vec(&view).unwrap();
    let decoded: DesktopRunView = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(view, decoded);
}

#[test]
fn active_run_view_with_web_surface_round_trips() {
    let view = DesktopRunView {
        project: "demo".to_owned(),
        branch: "main".to_owned(),
        head: "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        status: DesktopRunStatus::Active,
        surfaces: vec![DesktopSurfaceView::Web {
            url: "http://127.0.0.1:8000".to_owned(),
            profile: WEB_SURFACE_PROFILE.to_owned(),
        }],
    };
    let bytes = serde_json::to_vec(&view).unwrap();
    let decoded: DesktopRunView = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(view, decoded);
}

#[test]
fn inspect_request_is_strict() {
    let decoded: DesktopRunInspectRequest = serde_json::from_str(r#"{"project":"demo"}"#).unwrap();
    assert_eq!(decoded.project, "demo");
    assert!(
        serde_json::from_str::<DesktopRunInspectRequest>(r#"{"project":"demo","extra":1}"#)
            .is_err()
    );
}

#[test]
fn surface_kind_discriminates_web_from_terminal() {
    let web = r#"{"kind":"web","url":"http://127.0.0.1:1","profile":"ato.web-surface.v1"}"#;
    let terminal = r#"{"kind":"terminal","profile":"ato.terminal-surface.v1"}"#;
    let web: DesktopSurfaceView = serde_json::from_str(web).unwrap();
    let terminal: DesktopSurfaceView = serde_json::from_str(terminal).unwrap();
    assert!(matches!(web, DesktopSurfaceView::Web { .. }));
    assert!(matches!(terminal, DesktopSurfaceView::Terminal { .. }));
    assert!(
        serde_json::from_str::<DesktopSurfaceView>(
            r#"{"kind":"web","url":"http://127.0.0.1:1","profile":"p","extra":1}"#
        )
        .is_err()
    );
}
