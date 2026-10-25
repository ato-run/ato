//! Workload-owned Linux GUI producer. Lifecycle stays in the existing supervisor.
//!
//! TigerVNC creates a new X server rather than scraping an existing desktop.
//! RFB is loopback-only and has no credential: the Runner must use the existing
//! authenticated gateway from inside this network boundary (or an explicitly
//! isolated guest relay). Surface readiness is a first-frame observation, never K.

use std::io;

use serde::{Deserialize, Serialize};

use crate::supervisor::{ChildWorkload, SpawnPlan, Workload};

const PRODUCER: &str = include_str!("pixel-producer.sh");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PixelProducerConfig {
    pub width: u16,
    pub height: u16,
    pub rfb_port: u16,
    /// A private display within the workload's guest/container, not a host display.
    pub display: u16,
}

impl PixelProducerConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !(320..=4096).contains(&self.width) || !(200..=2160).contains(&self.height) {
            return Err("pixel_surface viewport must be between 320x200 and 4096x2160".into());
        }
        if self.rfb_port < 1024 || !(64..=4095).contains(&self.display) {
            return Err(
                "pixel_surface requires an unprivileged RFB port and a private display >=64".into(),
            );
        }
        Ok(())
    }

    fn wrap(&self, plan: &SpawnPlan) -> io::Result<SpawnPlan> {
        self.validate().map_err(io::Error::other)?;
        if plan.cmd.is_empty() {
            return Err(io::Error::other("pixel_surface app argv is empty"));
        }
        if plan.base_env.contains_key("DISPLAY") || plan.base_env.contains_key("XAUTHORITY") {
            return Err(io::Error::other(
                "pixel_surface owns DISPLAY and XAUTHORITY",
            ));
        }
        let mut wrapped = plan.clone();
        wrapped.cmd = vec![
            "/bin/bash".into(),
            "-c".into(),
            PRODUCER.into(),
            "ato-pixel-producer".into(),
            self.width.to_string(),
            self.height.to_string(),
            self.rfb_port.to_string(),
            self.display.to_string(),
        ];
        wrapped.cmd.extend(plan.cmd.iter().cloned());
        Ok(wrapped)
    }
}

pub struct PixelChildWorkload {
    config: PixelProducerConfig,
    child: ChildWorkload,
}

impl PixelChildWorkload {
    pub fn new(config: PixelProducerConfig) -> Self {
        Self {
            config,
            child: ChildWorkload::default(),
        }
    }
}

impl Workload for PixelChildWorkload {
    fn start(&mut self, plan: &SpawnPlan) -> io::Result<()> {
        self.child.start(&self.config.wrap(plan)?)
    }
    fn stop(&mut self) -> io::Result<bool> {
        self.child.stop()
    }
    fn is_running(&self) -> bool {
        self.child.is_running()
    }
    fn run_once(&mut self, _: &SpawnPlan) -> io::Result<i32> {
        Err(io::Error::other(
            "pixel_surface cannot execute a run_once task",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn config() -> PixelProducerConfig {
        PixelProducerConfig {
            width: 1280,
            height: 720,
            rfb_port: 5900,
            display: 99,
        }
    }
    fn plan() -> SpawnPlan {
        SpawnPlan {
            cmd: vec!["app".into(), "$(touch /should-not-execute)".into()],
            cwd: "/app".into(),
            base_env: BTreeMap::new(),
            secret_env: vec![("BYOK".into(), PathBuf::from("/run/ato/bindings/key"))],
            stdout_log: "out".into(),
            stderr_log: "err".into(),
            rootfs: None,
        }
    }
    #[test]
    fn producer_preserves_argv_binding_paths_and_rootfs_without_reading_secrets() {
        let mut input = plan();
        input.rootfs = Some("/opt/ato/services/app/rootfs".into());
        let wrapped = config().wrap(&input).unwrap();
        assert_eq!(&wrapped.cmd[8..], input.cmd);
        assert_eq!(wrapped.secret_env, input.secret_env);
        assert_eq!(wrapped.rootfs, input.rootfs);
        assert!(wrapped.base_env.is_empty());
        assert!(PRODUCER.contains("\"$@\" &"));
        assert!(PRODUCER.contains("-interface 127.0.0.1"));
        assert!(PRODUCER.contains("-AcceptCutText=0 -SendCutText=0"));
        assert!(!PRODUCER.contains("X0tigervnc"));
    }
    #[test]
    fn invalid_viewport_ports_displays_and_host_display_override_fail_closed() {
        let mut cfg = config();
        cfg.width = 0;
        assert!(cfg.validate().is_err());
        cfg = config();
        cfg.rfb_port = 80;
        assert!(cfg.validate().is_err());
        cfg = config();
        cfg.display = 0;
        assert!(cfg.validate().is_err());
        let mut input = plan();
        input.base_env.insert("DISPLAY".into(), ":0".into());
        assert!(config().wrap(&input).is_err());
        input = plan();
        input.cmd.clear();
        assert!(config().wrap(&input).is_err());
    }
    #[test]
    fn supervisor_rejects_pty_or_multiple_services_with_pixel_producer() {
        for json in [
            r#"{"stdio_mode":"pty","cmd":["app"],"pixel_surface":{"width":1280,"height":720,"rfb_port":5900,"display":99}}"#,
            r#"{"services":[{"name":"a","cmd":["app"]},{"name":"b","cmd":["app"]}],"pixel_surface":{"width":1280,"height":720,"rfb_port":5900,"display":99}}"#,
        ] {
            let cfg: crate::supervisor::SupervisorConfig = serde_json::from_str(json).unwrap();
            assert!(cfg.validate().is_err());
        }
        let cfg: crate::supervisor::SupervisorConfig = serde_json::from_str(
            r#"{"cmd":["app"],"pixel_surface":{"width":1280,"height":720,"rfb_port":5900,"display":99}}"#).unwrap();
        assert!(cfg.validate().is_ok());
    }
}
