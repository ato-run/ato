use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use snapshot_builder::static_web_bundle::produce_static_web_bundle;
use snapshot_builder::static_web_output::{
    StaticWebOutputPlan, extract_static_web_output_with_browser_runner,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BundlePlan {
    materialization_id: String,
    image_output_root: PathBuf,
    entry_path: String,
    spa_fallback: bool,
    connect_src: Vec<String>,
    browser_runner_bridge: bool,
}

fn main() -> Result<()> {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        bail!("usage: static-web-bundle <plan.json> <image-root> <destination-parent>");
    }
    let plan_path = Path::new(&args[0]);
    let image_root = Path::new(&args[1]);
    let destination_parent = Path::new(&args[2]);
    let input = fs::read(plan_path)
        .with_context(|| format!("read static web plan {}", plan_path.display()))?;
    let input: BundlePlan = serde_json::from_slice(&input)
        .with_context(|| format!("parse static web plan {}", plan_path.display()))?;
    let browser_runner_bridge = input.browser_runner_bridge;
    let plan = StaticWebOutputPlan {
        materialization_id: input.materialization_id,
        image_output_root: input.image_output_root,
        entry_path: input.entry_path,
        spa_fallback: input.spa_fallback,
        connect_src: input.connect_src,
    };
    let extracted =
        extract_static_web_output_with_browser_runner(image_root, &plan, browser_runner_bridge)?;
    let produced =
        produce_static_web_bundle(&plan, extracted.output_root(), destination_parent, &[])?;
    println!("{}", String::from_utf8(produced.receipt_bytes)?);
    Ok(())
}
