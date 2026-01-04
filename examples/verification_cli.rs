#[cfg(not(feature = "phase2"))]
fn main() {
    eprintln!("This example requires Cargo feature `phase2`.");
}

#[cfg(feature = "phase2")]
use capsuled_engine::proto::onescluster::coordinator::v1::coordinator_service_client::CoordinatorServiceClient;
#[cfg(feature = "phase2")]
use capsuled_engine::proto::onescluster::coordinator::v1::ListRequest;

#[cfg(feature = "phase2")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Wait for engine to start
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let mut client = CoordinatorServiceClient::connect("http://127.0.0.1:50051").await?;

    println!("Listing capsules...");
    let request = tonic::Request::new(ListRequest {});
    let response = client.list_capsules(request).await?;

    println!("RESPONSE={:?}", response);

    Ok(())
}
