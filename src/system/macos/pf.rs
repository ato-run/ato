use crate::config::EgressRuleEntry;
use crate::system::common::{IsolationRule, SystemError};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct PfAnchor {
    name: String,
    rules_path: PathBuf,
}

impl PfAnchor {
    pub fn create() -> Result<Self, SystemError> {
        let name = format!("nacelle_{}", std::process::id());
        let rules_path = std::env::temp_dir().join(format!("nacelle_pf_{}.conf", name));

        let anchor = Self { name, rules_path };
        anchor.load_anchor()?;
        Ok(anchor)
    }

    pub fn update_rules(&mut self, rule: &IsolationRule) -> Result<(), SystemError> {
        let rules = build_pf_rules(&self.name, rule)?;
        fs::write(&self.rules_path, rules).map_err(|e| SystemError::Anyhow(e.into()))?;
        self.reload_rules()
    }

    fn load_anchor(&self) -> Result<(), SystemError> {
        let anchor_rule = format!("anchor \"{}\"\n", self.name);
        let status = Command::new("pfctl")
            .args(["-a", &self.name, "-f", "-"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                if let Some(mut stdin) = child.stdin.take() {
                    use std::io::Write;
                    stdin.write_all(anchor_rule.as_bytes())?;
                }
                child.wait()
            })
            .map_err(|e| SystemError::Anyhow(e.into()))?;

        if !status.success() {
            return Err(SystemError::Anyhow(anyhow::anyhow!(
                "pfctl failed to load anchor {} (exit={})",
                self.name,
                status.code().unwrap_or(1)
            )));
        }

        Ok(())
    }

    fn reload_rules(&self) -> Result<(), SystemError> {
        let status = Command::new("pfctl")
            .args(["-a", &self.name, "-f"])
            .arg(&self.rules_path)
            .status()
            .map_err(|e| SystemError::Anyhow(e.into()))?;

        if !status.success() {
            return Err(SystemError::Anyhow(anyhow::anyhow!(
                "pfctl failed to reload anchor {} (exit={})",
                self.name,
                status.code().unwrap_or(1)
            )));
        }

        Ok(())
    }
}

impl Drop for PfAnchor {
    fn drop(&mut self) {
        let _ = Command::new("pfctl")
            .args(["-a", &self.name, "-F", "all"])
            .status();
        let _ = fs::remove_file(&self.rules_path);
    }
}

fn build_pf_rules(_anchor_name: &str, rule: &IsolationRule) -> Result<String, SystemError> {
    let mut rules = String::new();
    rules.push_str("block drop out all\n");
    rules.push_str("pass out inet proto udp to any port 53\n");

    for entry in &rule.allow_rules {
        rules.push_str(&format!("pass out to {}\n", format_pf_target(entry)?));
    }

    Ok(rules)
}

fn format_pf_target(rule: &EgressRuleEntry) -> Result<String, SystemError> {
    match rule.rule_type.as_str() {
        "ip" => Ok(rule.value.clone()),
        "cidr" => Ok(rule.value.clone()),
        "domain" => Ok(rule.value.clone()),
        other => Err(SystemError::InvalidConfig(format!(
            "Unsupported egress rule type for PF: {}",
            other
        ))),
    }
}
