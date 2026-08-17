use crate::system::common::SystemError;
use std::process::Command;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[derive(Debug, Clone)]
pub struct GroupIdentity {
    name: String,
    gid: u32,
}

impl GroupIdentity {
    pub fn acquire_ephemeral() -> Result<Self, SystemError> {
        let name = format!("nacelle_{}", std::process::id());
        let _ = Command::new("dseditgroup")
            .args(["-o", "delete", "-q", &name])
            .status();

        let mut cmd = Command::new("dseditgroup");
        cmd.args(["-o", "create", "-q", &name]);
        run_command(cmd, "Failed to create group")?;

        let gid = read_group_gid(&name)?;
        Ok(Self { name, gid })
    }

    pub fn gid(&self) -> u32 {
        self.gid
    }

    pub fn apply_to_child(&self, cmd: &mut Command) -> Result<(), SystemError> {
        #[cfg(unix)]
        {
            cmd.gid(self.gid);
        }
        Ok(())
    }
}

impl Drop for GroupIdentity {
    fn drop(&mut self) {
        let _ = Command::new("dseditgroup")
            .args(["-o", "delete", "-q", &self.name])
            .status();
    }
}

fn read_group_gid(name: &str) -> Result<u32, SystemError> {
    let output = Command::new("dscl")
        .args([".", "-read", &format!("/Groups/{}", name), "PrimaryGroupID"])
        .output()
        .map_err(|e| SystemError::Anyhow(e.into()))?;

    if !output.status.success() {
        return Err(SystemError::Anyhow(anyhow::anyhow!(
            "Failed to read group gid for {} (exit={})",
            name,
            output.status.code().unwrap_or(1)
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let gid = stdout
        .split_whitespace()
        .last()
        .ok_or_else(|| SystemError::Anyhow(anyhow::anyhow!("Missing gid for {}", name)))?;
    let gid: u32 = gid
        .parse()
        .map_err(|e| SystemError::Anyhow(anyhow::anyhow!("Invalid gid {}: {}", gid, e)))?;
    Ok(gid)
}

fn run_command(mut cmd: Command, context: &str) -> Result<(), SystemError> {
    let status = cmd.status().map_err(|e| SystemError::Anyhow(e.into()))?;
    if !status.success() {
        return Err(SystemError::Anyhow(anyhow::anyhow!(
            "{} (exit={})",
            context,
            status.code().unwrap_or(1)
        )));
    }
    Ok(())
}
