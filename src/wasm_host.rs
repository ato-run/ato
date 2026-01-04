use anyhow::{anyhow, Result};
use std::sync::{Arc, Mutex};
use tracing::{info, warn};
use wasmtime::*;

/// AdepLogic wraps the Wasmtime runtime and provides manifest validation
pub struct AdepLogic {
    engine: Engine,
    module: Module,
}

impl AdepLogic {
    /// Create a new AdepLogic instance from Wasm bytes
    pub fn new(wasm_bytes: &[u8]) -> Result<Self> {
        info!("Initializing Wasm runtime for adep-logic");

        let engine = Engine::default();
        let module = Module::new(&engine, wasm_bytes)?;

        Ok(Self { engine, module })
    }

    /// Load AdepLogic from a file path
    pub fn from_file(path: &str) -> Result<Self> {
        info!("Loading Wasm module from: {}", path);
        let wasm_bytes = std::fs::read(path)?;
        Self::new(&wasm_bytes)
    }

    /// Validate a manifest (adep.json) using the Wasm module
    ///
    /// # Arguments
    /// * `adep_json` - JSON string of the manifest to validate
    ///
    /// # Returns
    /// * `Ok(())` - Validation succeeded
    /// * `Err(...)` - Validation failed with error message
    pub fn validate_manifest(&self, adep_json: &str) -> Result<()> {
        info!("Validating manifest ({} bytes)", adep_json.len());

        // Create a new store for this execution
        let mut store = Store::new(&self.engine, ());

        // Instantiate the module
        let instance = Instance::new(&mut store, &self.module, &[])?;

        // Get the validate_manifest function
        let validate_fn = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "validate_manifest")
            .map_err(|e| anyhow!("Failed to get validate_manifest function: {}", e))?;

        // Get the alloc function for safe memory allocation
        let alloc_fn = instance
            .get_typed_func::<i32, i32>(&mut store, "alloc")
            .map_err(|e| anyhow!("Failed to get alloc function: {}", e))?;

        // Allocate memory in Wasm linear memory for the JSON string
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| anyhow!("Wasm module has no exported memory"))?;

        // Write JSON to Wasm memory
        let json_bytes = adep_json.as_bytes();
        let json_len = json_bytes.len();

        // Allocate memory in Wasm
        let json_ptr = alloc_fn
            .call(&mut store, json_len as i32)
            .map_err(|e| anyhow!("Failed to allocate Wasm memory: {}", e))?;

        // Write data to the allocated memory
        memory
            .write(&mut store, json_ptr as usize, json_bytes)
            .map_err(|e| anyhow!("Failed to write to Wasm memory: {}", e))?;

        // Call the validation function
        let result = validate_fn
            .call(&mut store, (json_ptr, json_len as i32))
            .map_err(|e| anyhow!("Wasm function call failed: {}", e))?;

        // Note: In a production environment, we should also call `dealloc` here to prevent memory leaks
        // inside the Wasm instance if the instance is long-lived.
        // For now, since we might be creating a new store/instance or the logic is simple, we skip it,
        // but it is recommended to add it.

        if result == 1 {
            info!("Manifest validation succeeded");
            Ok(())
        } else {
            warn!("Manifest validation failed");
            Err(anyhow!("Manifest validation failed"))
        }
    }
}

/// Thread-safe wrapper for AdepLogic
pub struct AdepLogicHost {
    logic: Arc<Mutex<AdepLogic>>,
}

impl AdepLogicHost {
    pub fn new(wasm_bytes: &[u8]) -> Result<Self> {
        let logic = AdepLogic::new(wasm_bytes)?;
        Ok(Self {
            logic: Arc::new(Mutex::new(logic)),
        })
    }

    pub fn from_file(path: &str) -> Result<Self> {
        let logic = AdepLogic::from_file(path)?;
        Ok(Self {
            logic: Arc::new(Mutex::new(logic)),
        })
    }

    pub fn validate_manifest(&self, adep_json: &str) -> Result<()> {
        let logic = self
            .logic
            .lock()
            .map_err(|e| anyhow!("Lock error: {}", e))?;
        logic.validate_manifest(adep_json)
    }

    pub fn clone_logic(&self) -> Arc<Mutex<AdepLogic>> {
        Arc::clone(&self.logic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_WASM_PATH: &str =
        "../adep-logic/target/wasm32-unknown-unknown/release/adep_logic.wasm";

    #[test]
    fn test_validate_valid_manifest() {
        if !std::path::Path::new(TEST_WASM_PATH).exists() {
            println!("Skipping test: Wasm file not found at {}", TEST_WASM_PATH);
            return;
        }

        let logic = AdepLogic::from_file(TEST_WASM_PATH).unwrap();
        let valid_json =
            r#"{"name":"test-capsule","version":"1.0.0","compute":{"image":"alpine:latest"}}"#;

        let result = logic.validate_manifest(valid_json);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_invalid_manifest() {
        if !std::path::Path::new(TEST_WASM_PATH).exists() {
            println!("Skipping test: Wasm file not found at {}", TEST_WASM_PATH);
            return;
        }

        let logic = AdepLogic::from_file(TEST_WASM_PATH).unwrap();
        let invalid_json = r#"{"name":"","version":"1.0.0"}"#; // Empty name

        let result = logic.validate_manifest(invalid_json);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_malformed_json() {
        if !std::path::Path::new(TEST_WASM_PATH).exists() {
            println!("Skipping test: Wasm file not found at {}", TEST_WASM_PATH);
            return;
        }

        let logic = AdepLogic::from_file(TEST_WASM_PATH).unwrap();
        let malformed_json = r#"{"name":"test"#; // Malformed JSON

        let result = logic.validate_manifest(malformed_json);
        assert!(result.is_err());
    }
}
