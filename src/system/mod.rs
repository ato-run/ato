//! System-level modules for platform-specific utilities and security.
//!
//! This module provides a cross-platform abstraction layer (Tauri-style):
//! - Linux: eBPF + cgroup v2
//! - macOS: PF + group-based rules (planned)
//! - Windows: WFP + AppContainer (planned)
//!
//! v3.0: This module contains OS-native sandbox enforcement and verification
//! utilities. Validation/policy resolution has been moved to capsule-cli.
//!
//! Remaining components:
//! - sandbox: OS-native process sandboxing (Landlock/Seatbelt)
//! - path: Path validation and security
//!
//! Moved to capsule-cli:
//! - verifier: L1 Source Policy + L2 Signature Verification
//! - signing: Ed25519 signature generation
//! - egress_policy: L4 Egress policy resolution (domain → IP)
//!
//! Note: Audit logging is handled by the caller (capsule-cli).

use async_trait::async_trait;
use std::process::Command;

pub mod common;
#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

pub mod path;
pub mod sandbox;
pub mod vram;
pub use path::*;
pub use vram::*;

use common::{IsolationRule, SystemError};

/// OSごとの隔離バックエンドが実装すべきインターフェース。
#[async_trait]
pub trait NetworkSandbox: Send + Sync {
    /// サンドボックス環境の準備 (cgroup作成, WFPセッション開始, PF Anchor作成)
    async fn prepare(&mut self, rule: IsolationRule) -> Result<(), SystemError>;

    /// 子プロセスへの適用 (OS固有の設定を注入)
    fn apply_to_child(&self, cmd: &mut Command) -> Result<(), SystemError>;

    /// 動的なルール更新 (DNS TTL切れによるIPリスト更新など)
    async fn update_rules(&mut self, rule: IsolationRule) -> Result<(), SystemError>;
}

/// OSを自動判別してサンドボックス実装を返す。
pub fn new_network_sandbox() -> Box<dyn NetworkSandbox> {
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxSandbox::new())
    }

    #[cfg(target_os = "windows")]
    {
        Box::new(windows::WindowsSandbox::new())
    }

    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacosSandbox::new())
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        struct UnsupportedSandbox;
        #[async_trait]
        impl NetworkSandbox for UnsupportedSandbox {
            async fn prepare(&mut self, _rule: IsolationRule) -> Result<(), SystemError> {
                Err(SystemError::Unsupported(
                    "network sandbox not supported on this platform".to_string(),
                ))
            }

            fn apply_to_child(&self, _cmd: &mut Command) -> Result<(), SystemError> {
                Err(SystemError::Unsupported(
                    "network sandbox not supported on this platform".to_string(),
                ))
            }

            async fn update_rules(&mut self, _rule: IsolationRule) -> Result<(), SystemError> {
                Err(SystemError::Unsupported(
                    "network sandbox not supported on this platform".to_string(),
                ))
            }
        }
        Box::new(UnsupportedSandbox)
    }
}
