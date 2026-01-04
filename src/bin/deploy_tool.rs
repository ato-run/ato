use anyhow::Result;
use clap::Parser;

use capsuled::proto::onescluster::engine::v1::engine_client::EngineClient;
use capsuled::proto::onescluster::engine::v1::{deploy_request::Manifest, DeployRequest};

#[derive(Parser, Debug)]
#[command(about = "Deploy a capsule to the Engine")]
struct Args {
    /// Path to capsule manifest (TOML or JSON)
    #[arg(short, long)]
    manifest: String,

    /// Engine gRPC endpoint
    #[arg(short, long, default_value = "http://localhost:50051")]
    endpoint: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Read manifest
    let manifest_content = std::fs::read_to_string(&args.manifest)?;
    println!("📦 Deploying capsule from: {}", args.manifest);
    println!("🌐 Connecting to Engine at: {}", args.endpoint);

    // Connect to Engine
    let mut client = EngineClient::connect(args.endpoint.clone()).await?;
    println!("✅ Connected to Engine");

    // Send deploy request
    let request = tonic::Request::new(DeployRequest {
        capsule_id: "cloud-burst-test".to_string(),
        manifest: Some(Manifest::AdepJson(manifest_content.as_bytes().to_vec())),
        oci_image: "".to_string(),
        digest: "".to_string(),
        manifest_signature: vec![],
    });

    println!("🚀 Sending deployment request...");
    let response = client.deploy_capsule(request).await?;
    let resp = response.into_inner();

    println!("✅ Deployment initiated!");
    println!("   Capsule ID: {}", resp.capsule_id);
    println!("   Local URL: {}", resp.local_url);

    Ok(())
}
