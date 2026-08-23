use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use ato_runtime_object_graph::{ValidatorAgent, ValidatorAgentConfig, ValidatorRunOutcome};
use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "ato-runtime-validator")]
struct Args {
    #[arg(long, env = "CAPSULE_VALIDATOR_API_URL")]
    api_url: String,
    #[arg(long, env = "CAPSULE_VALIDATOR_AGENT_TOKEN", hide_env_values = true)]
    token: String,
    #[arg(long, env = "CAPSULE_VALIDATOR_AGENT_ID")]
    agent_id: String,
    #[arg(long, env = "CAPSULE_VALIDATOR_WORK_ROOT")]
    work_root: PathBuf,
    #[arg(long, default_value_t = 1000)]
    poll_interval_ms: u64,
    #[arg(long)]
    once: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let agent = ValidatorAgent::new(ValidatorAgentConfig {
        api_url: args.api_url,
        token: args.token,
        agent_id: args.agent_id,
        work_root: args.work_root,
        poll_interval: Duration::from_millis(args.poll_interval_ms),
    })?;
    if args.once {
        match agent.run_once()? {
            ValidatorRunOutcome::Idle => println!("idle"),
            ValidatorRunOutcome::Verified {
                graph_id,
                bundle_id,
            } => {
                println!("verified graph={graph_id} bundle={bundle_id}")
            }
            ValidatorRunOutcome::Rejected {
                graph_id,
                rejection_code,
            } => println!("rejected graph={graph_id} code={rejection_code}"),
        }
        return Ok(());
    }
    agent.run_forever()
}
