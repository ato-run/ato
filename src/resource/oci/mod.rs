pub mod cache;
pub mod image;
pub mod layer;
pub mod registry;
/// OCI (Open Container Initiative) runtime specification module
///
/// This module handles:
/// - Generation of OCI config.json files for capsule execution
/// - Docker Registry API v2 client for image pulling
/// - Layer extraction and caching
/// - Image management (pull, cache, rootfs preparation)
pub mod spec_builder;

pub use cache::{CacheError, LayerCache};
pub use image::{ImageError, ImageManager, PulledImage};
pub use layer::{LayerError, LayerExtractor};
pub use registry::{ImageManifest, ImageRef, RegistryClient, RegistryError};
pub use spec_builder::build_oci_spec;
