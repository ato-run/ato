const DOCKERFILE: &str = include_str!("../fixtures/linux-x11-pixel/Dockerfile");
const START_SCRIPT: &str = include_str!("../fixtures/linux-x11-pixel/start-pixel-fixture.sh");
const HEALTH_SCRIPT: &str = include_str!("../fixtures/linux-x11-pixel/health.py");

#[test]
fn fixture_pins_linux_image_and_declares_the_curated_mvp_stack() {
    let first_line = DOCKERFILE.lines().next().expect("Dockerfile has FROM");
    assert!(first_line.starts_with("FROM docker.io/library/ubuntu:24.04@sha256:"));
    for component in ["xvfb", "openbox", "x11vnc", "xterm"] {
        assert!(
            DOCKERFILE.to_ascii_lowercase().contains(component),
            "missing {component}"
        );
    }
    assert!(START_SCRIPT.contains("1280x720x24"));
    assert!(START_SCRIPT.contains("setxkbmap -display \"$DISPLAY\" us"));
    assert!(START_SCRIPT.contains("-wait 33"));
}

#[test]
fn build_readiness_is_bound_to_the_target_window_and_framebuffer() {
    for evidence in [
        "kill -0 \"$APP_PID\"",
        "--pid \"$APP_PID\"",
        "--class AtoPixelFixture",
        "WM_CLASS",
        "Map State: IsViewable",
        "BEFORE_HASH",
        "AFTER_HASH",
    ] {
        assert!(
            START_SCRIPT.contains(evidence),
            "missing readiness {evidence}"
        );
    }
    assert!(HEALTH_SCRIPT.contains("os.kill(APP_PID, 0)"));
    assert!(HEALTH_SCRIPT.contains("AtoPixelFixture"));
    assert!(HEALTH_SCRIPT.contains("Map State: IsViewable"));
}

#[test]
fn fixture_generates_no_session_or_vnc_credentials_before_seal() {
    let executable_fixture = format!("{DOCKERFILE}\n{START_SCRIPT}\n{HEALTH_SCRIPT}");
    for forbidden_marker in [
        "VNC_PASSWORD",
        "-rfbauth",
        "app_view_token",
        "websocket_token",
        "session_secret",
    ] {
        assert!(
            !executable_fixture.contains(forbidden_marker),
            "fixture must not generate or embed {forbidden_marker}"
        );
    }
    assert!(START_SCRIPT.contains("-nopw"));
    assert!(START_SCRIPT.contains("-noclipboard"));
    assert!(START_SCRIPT.contains("-nosetclipboard"));
}
