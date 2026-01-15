use anyhow::Result;
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use anyhow::Context;
#[cfg(target_os = "linux")]
use std::net::IpAddr;

#[cfg(target_os = "linux")]
use crate::egress::MAX_EGRESS_RULES;
use crate::runtime_config::EgressRuleEntry;

#[cfg(target_os = "linux")]
use aya::maps::lpm_trie::{Key as LpmKey, LpmTrie};
#[cfg(target_os = "linux")]
use aya::programs::{CgroupAttachMode, CgroupSkb, CgroupSkbAttachType};
#[cfg(target_os = "linux")]
use aya::Pod;
#[cfg(target_os = "linux")]
use aya::{include_bytes_aligned, Ebpf};

pub struct EgressEnforcerHandle {
    #[cfg(target_os = "linux")]
    bpf: Ebpf,
    pub cgroup_path: PathBuf,
}

impl EgressEnforcerHandle {
    pub fn update_allowlist(&mut self, rules: &[EgressRuleEntry]) -> Result<()> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = rules;
            anyhow::bail!("eBPF enforcer is only supported on Linux");
        }

        #[cfg(target_os = "linux")]
        {
            load_rules(&mut self.bpf, "IPV4_ALLOW", "IPV6_ALLOW", rules)
        }
    }
}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Clone, Copy)]
struct Ipv4LpmKey {
    addr: u32,
}

#[cfg(target_os = "linux")]
unsafe impl Pod for Ipv4LpmKey {}

#[cfg(target_os = "linux")]
#[repr(C)]
#[derive(Clone, Copy)]
struct Ipv6LpmKey {
    addr: [u8; 16],
}

#[cfg(target_os = "linux")]
unsafe impl Pod for Ipv6LpmKey {}

pub fn start_enforcer(
    rules: &[EgressRuleEntry],
    dns_rules: &[EgressRuleEntry],
    job_id: &str,
) -> Result<EgressEnforcerHandle> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (rules, dns_rules, job_id);
        anyhow::bail!("eBPF enforcer is only supported on Linux");
    }

    #[cfg(target_os = "linux")]
    {
        if rules.len() + dns_rules.len() > MAX_EGRESS_RULES {
            anyhow::bail!(
                "Egress allowlist exceeds {} entries (fail-closed)",
                MAX_EGRESS_RULES
            );
        }

        let cgroup_path = create_cgroup(job_id)?;

        let mut bpf = Ebpf::load(include_bytes_aligned!(concat!(
            env!("OUT_DIR"),
            "/nacelle-ebpf"
        )))
        .context("Failed to load eBPF object")?;

        let program: &mut CgroupSkb = bpf
            .program_mut("nacelle_egress")
            .context("eBPF program not found")?
            .try_into()?;
        program.load()?;
        let cgroup = std::fs::File::open(&cgroup_path)
            .with_context(|| format!("Failed to open cgroup: {}", cgroup_path.display()))?;
        program.attach(
            cgroup,
            CgroupSkbAttachType::Egress,
            CgroupAttachMode::Single,
        )?;

        load_rules(&mut bpf, "IPV4_ALLOW", "IPV6_ALLOW", rules)?;
        load_rules(&mut bpf, "DNS_IPV4_ALLOW", "DNS_IPV6_ALLOW", dns_rules)?;

        Ok(EgressEnforcerHandle { bpf, cgroup_path })
    }
}

#[cfg(target_os = "linux")]
fn create_cgroup(job_id: &str) -> Result<PathBuf> {
    let root = PathBuf::from("/sys/fs/cgroup/nacelle");
    std::fs::create_dir_all(&root)?;
    let cgroup_path = root.join(job_id);
    std::fs::create_dir_all(&cgroup_path)?;
    Ok(cgroup_path)
}

#[cfg(target_os = "linux")]
fn load_rules(bpf: &mut Ebpf, v4_map: &str, v6_map: &str, rules: &[EgressRuleEntry]) -> Result<()> {
    let mut v4_entries: Vec<(u32, u32)> = Vec::new();
    let mut v6_entries: Vec<(u32, [u8; 16])> = Vec::new();

    for rule in rules {
        match rule.rule_type.as_str() {
            "ip" => {
                let ip: IpAddr = rule.value.parse()?;
                match ip {
                    IpAddr::V4(v4addr) => {
                        v4_entries.push((32, u32::from_be_bytes(v4addr.octets())));
                    }
                    IpAddr::V6(v6addr) => {
                        v6_entries.push((128, v6addr.octets()));
                    }
                }
            }
            "cidr" => {
                let (addr, prefix) = rule
                    .value
                    .split_once('/')
                    .ok_or_else(|| anyhow::anyhow!("Invalid CIDR: {}", rule.value))?;
                let prefix: u32 = prefix.parse()?;
                let ip: IpAddr = addr.parse()?;
                match ip {
                    IpAddr::V4(v4addr) => {
                        v4_entries.push((prefix, u32::from_be_bytes(v4addr.octets())));
                    }
                    IpAddr::V6(v6addr) => {
                        v6_entries.push((prefix, v6addr.octets()));
                    }
                }
            }
            other => anyhow::bail!("Unsupported rule type: {}", other),
        }
    }

    {
        let map_v4 = bpf
            .map_mut(v4_map)
            .ok_or_else(|| anyhow::anyhow!("Missing eBPF map: {}", v4_map))?;
        let mut v4: LpmTrie<_, Ipv4LpmKey, u8> = LpmTrie::try_from(map_v4)?;
        for (prefix, addr) in v4_entries {
            let key = LpmKey::new(prefix, Ipv4LpmKey { addr });
            v4.insert(&key, 1, 0)?;
        }
    }

    {
        let map_v6 = bpf
            .map_mut(v6_map)
            .ok_or_else(|| anyhow::anyhow!("Missing eBPF map: {}", v6_map))?;
        let mut v6: LpmTrie<_, Ipv6LpmKey, u8> = LpmTrie::try_from(map_v6)?;
        for (prefix, addr) in v6_entries {
            let key = LpmKey::new(prefix, Ipv6LpmKey { addr });
            v6.insert(&key, 1, 0)?;
        }
    }

    Ok(())
}
