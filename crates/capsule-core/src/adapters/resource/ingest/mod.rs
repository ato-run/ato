//! Resource Ingestion
//!
//! Handles fetching external resources (HTTP, S3, etc.) and ingesting them into CAS

pub mod fetcher;
pub mod http;

pub use fetcher::{fetch_resource, FetcherConfig, ResourceFetchRequest, ResourceFetchResult};
