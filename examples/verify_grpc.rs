#[cfg(not(feature = "phase2"))]
fn main() {
    eprintln!("This example requires Cargo feature `phase2`.");
}

#[cfg(feature = "phase2")]
use capsuled_engine::proto::onescluster::engine::v1::engine_client::EngineClient;
#[cfg(feature = "phase2")]
use capsuled_engine::proto::onescluster::engine::v1::{
    GetResourcesRequest, GetSystemStatusRequest,
};
#[cfg(feature = "phase2")]
use colored::*;

#[cfg(feature = "phase2")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "http://127.0.0.1:50051";
    println!("{}", format!("🔌 Connecting to Engine at {}", addr).cyan());

    let mut client = EngineClient::connect(addr).await?;

    println!("\n{}", "📊 Step 1: GetResources".bold());
    let resources = client
        .get_resources(tonic::Request::new(GetResourcesRequest {}))
        .await?
        .into_inner();
    println!("{}", format!("Resources: {:?}", resources).green());

    println!("\n{}", "🧭 Step 2: GetSystemStatus".bold());
    let status = client
        .get_system_status(tonic::Request::new(GetSystemStatusRequest {}))
        .await?
        .into_inner();
    println!("{}", format!("System status: {:?}", status).green());

    println!("\n{}", "✅ Verification Complete".bold().green());

    Ok(())
}
