use capsule_sync::{
    decode_payload_base64, GuestContextRole, GuestMode, GuestSession, WidgetBounds,
};

use crate::sync::SyncRuntime;
use serde_json::Value;
use std::path::PathBuf;

pub struct GuestManager {
    sync_runtime: Option<SyncRuntime>,
    session: Option<GuestSession>,
}

impl GuestManager {
    pub fn new() -> Self {
        Self {
            sync_runtime: None,
            session: None,
        }
    }

    pub fn attach(&mut self, sync_path: PathBuf) -> anyhow::Result<()> {
        let runtime = SyncRuntime::open(&sync_path)?;
        self.sync_runtime = Some(runtime);

        let mut session =
            GuestSession::new(sync_path).map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
        session.mode = GuestMode::Widget;
        session.role = GuestContextRole::Consumer;
        self.session = Some(session);

        Ok(())
    }

    pub fn as_widget(&mut self, host_app: &str) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            session
                .as_widget(host_app)
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
        }
        Ok(())
    }

    pub fn as_headless(&mut self, host_app: &str) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            session
                .as_headless(host_app)
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
        }
        Ok(())
    }

    pub fn as_consumer(&mut self) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            session
                .as_consumer()
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
        }
        Ok(())
    }

    pub fn as_owner(&mut self) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            session
                .as_owner()
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
        }
        Ok(())
    }

    pub fn grant_read_payload(&mut self) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            session
                .grant_read_payload()
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
        }
        Ok(())
    }

    pub fn grant_read_context(&mut self) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            session
                .grant_read_context()
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
        }
        Ok(())
    }

    pub fn grant_write_payload(&mut self) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            session
                .grant_write_payload()
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
        }
        Ok(())
    }

    pub fn grant_context_write(&mut self) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            session
                .grant_context_write()
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
        }
        Ok(())
    }

    pub fn grant_wasm_execution(&mut self) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            session
                .grant_wasm_execution()
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
        }
        Ok(())
    }

    pub fn revoke_wasm_execution(&mut self) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            session
                .revoke_wasm_execution()
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
        }
        Ok(())
    }

    pub fn allow_host(&mut self, host: &str) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            session
                .allow_host(host)
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
        }
        Ok(())
    }

    pub fn allow_env_var(&mut self, env_var: &str) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            session
                .allow_env_var(env_var)
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
        }
        Ok(())
    }

    pub fn set_cpu_limit_ms(&mut self, limit_ms: u64) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            session
                .set_cpu_limit_ms(limit_ms)
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
        }
        Ok(())
    }

    pub fn set_memory_limit_mb(&mut self, limit_mb: u64) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            session
                .set_memory_limit_mb(limit_mb)
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
        }
        Ok(())
    }

    pub fn set_widget_bounds(&mut self, bounds: WidgetBounds) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            session
                .set_widget_bounds(bounds)
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
        }
        Ok(())
    }

    pub fn execute_read_payload(&mut self) -> anyhow::Result<String> {
        if let Some(session) = &mut self.session {
            let response = session
                .execute_read_payload()
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
            if response.ok {
                if let Some(result) = response.result {
                    return Ok(serde_json::to_string(&result)?);
                }
            }
            return Err(anyhow::anyhow!(
                "Read failed: {}",
                response.error.map(|e| e.message).unwrap_or_default()
            ));
        }
        Err(anyhow::anyhow!("No active session"))
    }

    pub fn execute_read_payload_bytes(&mut self) -> anyhow::Result<Vec<u8>> {
        if let Some(session) = &mut self.session {
            return session
                .execute_read_payload_bytes()
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)));
        }
        Err(anyhow::anyhow!("No active session"))
    }

    pub fn execute_read_context(&mut self) -> anyhow::Result<String> {
        if let Some(session) = &mut self.session {
            let response = session
                .execute_read_context()
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
            if response.ok {
                if let Some(result) = response.result {
                    return Ok(serde_json::to_string(&result)?);
                }
            }
            return Err(anyhow::anyhow!(
                "Read context failed: {}",
                response.error.map(|e| e.message).unwrap_or_default()
            ));
        }
        Err(anyhow::anyhow!("No active session"))
    }

    pub fn execute_write_payload(&mut self, new_content: String) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            let response = session
                .execute_write_payload(new_content)
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
            if !response.ok {
                return Err(anyhow::anyhow!(
                    "Write failed: {}",
                    response.error.map(|e| e.message).unwrap_or_default()
                ));
            }
        }
        Ok(())
    }

    pub fn execute_update_payload(&mut self, new_content: String) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            let response = session
                .execute_update_payload(new_content)
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
            if !response.ok {
                return Err(anyhow::anyhow!(
                    "Update failed: {}",
                    response.error.map(|e| e.message).unwrap_or_default()
                ));
            }
        }
        Ok(())
    }

    pub fn execute_write_context(&mut self, new_context: serde_json::Value) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            let response = session
                .execute_write_context(new_context)
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
            if !response.ok {
                return Err(anyhow::anyhow!(
                    "Write context failed: {}",
                    response.error.map(|e| e.message).unwrap_or_default()
                ));
            }
        }
        Ok(())
    }

    pub fn execute_wasm(&mut self) -> anyhow::Result<()> {
        if let Some(session) = &mut self.session {
            let response = session
                .execute_wasm()
                .map_err(|e| anyhow::anyhow!(format!("{:?}", e)))?;
            if !response.ok {
                return Err(anyhow::anyhow!(
                    "Wasm execution failed: {}",
                    response.error.map(|e| e.message).unwrap_or_default()
                ));
            }

            if let Some(result) = response.result {
                if let Value::String(new_payload) = result {
                    if let Some(runtime) = &mut self.sync_runtime {
                        let new_bytes = decode_payload_base64(&new_payload)
                            .map_err(|e| anyhow::anyhow!(e.message))?;
                        runtime.update_payload(&new_bytes)?;
                    }
                }
            }
        }

        Ok(())
    }

    pub fn detach(&mut self) -> anyhow::Result<()> {
        self.session = None;
        Ok(())
    }
}
