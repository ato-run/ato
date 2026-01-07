#[cfg(test)]
mod tests {

    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    use capsuled::capnp_to_manifest::manifest_to_capnp_bytes;
    use capsuled::capsule_types::capsule_v1::{
        CapsuleExecution, CapsuleManifestV1, CapsuleRequirements, CapsuleRouting, CapsuleStorage,
        CapsuleType, RuntimeType,
    };
    use capsuled::security::verifier::ManifestVerifier;
    use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
    use rand::rngs::OsRng;

    /// Creates a valid test manifest for signature testing
    fn create_test_manifest(name: &str) -> CapsuleManifestV1 {
        CapsuleManifestV1 {
            schema_version: "1.0".to_string(),
            name: name.to_string(),
            version: "1.0.0".to_string(),
            capsule_type: CapsuleType::App,
            metadata: Default::default(),
            capabilities: None,
            requirements: CapsuleRequirements::default(),
            execution: CapsuleExecution {
                runtime: RuntimeType::Docker,
                entrypoint: "test:latest".to_string(),
                port: None,
                health_check: None,
                startup_timeout: 60,
                env: Default::default(),
                signals: Default::default(),
            },
            storage: CapsuleStorage::default(),
            routing: CapsuleRouting::default(),
            network: None,
            model: None,
            transparency: None,
            pool: None,
            targets: None,
        }
    }

    #[test]
    fn test_verifier_canonical_signing() {
        // 1. Generate a "Trusted" keypair
        let mut csprng = OsRng;
        let trusted_signing_key = SigningKey::generate(&mut csprng);
        let trusted_verifying_key = trusted_signing_key.verifying_key();
        let trusted_pubkey_fingerprint = format!(
            "ed25519:{}",
            BASE64.encode(trusted_verifying_key.as_bytes())
        );

        // 2. Create a valid manifest
        let manifest = create_test_manifest("safe-capsule");

        // 3. Generate CANONICAL bytes (Cap'n Proto format) for signing
        let canonical_bytes =
            manifest_to_capnp_bytes(&manifest).expect("Failed to generate canonical bytes");

        // 4. Sign the CANONICAL bytes
        let signature = trusted_signing_key.sign(&canonical_bytes);

        // 5. Create signature file structure (mocking libadep format)
        let create_sig_bytes = |pk: &VerifyingKey, sig: &ed25519_dalek::Signature| -> Vec<u8> {
            let mut buffer = Vec::new();
            buffer.push(0x01); // Version
            buffer.push(0x01); // KeyType
            buffer.extend_from_slice(pk.as_bytes());
            buffer.extend_from_slice(&sig.to_bytes());
            let metadata = b"{}";
            buffer.extend_from_slice(&(metadata.len() as u16).to_be_bytes());
            buffer.extend_from_slice(metadata);
            buffer
        };

        let sig_bytes = create_sig_bytes(&trusted_verifying_key, &signature);

        let verifier = ManifestVerifier::new(Some(trusted_pubkey_fingerprint.clone()), true);

        // Case A: Verify via verify_manifest (direct struct) -> OK
        assert!(
            verifier.verify_manifest(&manifest, &sig_bytes, "").is_ok(),
            "verify_manifest should succeed with correctly signed canonical bytes"
        );

        // Case B: Verify via verify (JSON input) -> OK
        // JSON will be parsed, then converted to canonical bytes for verification
        let json_content = serde_json::to_vec(&manifest).expect("JSON serialization failed");
        assert!(
            verifier.verify(&json_content, &sig_bytes, "").is_ok(),
            "verify with JSON input should succeed (converts to canonical bytes internally)"
        );
    }

    #[test]
    fn test_verifier_rejects_untrusted_signer() {
        // 1. Generate keypairs
        let mut csprng = OsRng;
        let trusted_signing_key = SigningKey::generate(&mut csprng);
        let trusted_verifying_key = trusted_signing_key.verifying_key();
        let trusted_pubkey_fingerprint = format!(
            "ed25519:{}",
            BASE64.encode(trusted_verifying_key.as_bytes())
        );

        let malicious_signing_key = SigningKey::generate(&mut csprng);
        let malicious_verifying_key = malicious_signing_key.verifying_key();

        // 2. Create and sign manifest with MALICIOUS key
        let manifest = create_test_manifest("hacked-capsule");
        let canonical_bytes =
            manifest_to_capnp_bytes(&manifest).expect("Failed to generate canonical bytes");
        let signature = malicious_signing_key.sign(&canonical_bytes);

        let create_sig_bytes = |pk: &VerifyingKey, sig: &ed25519_dalek::Signature| -> Vec<u8> {
            let mut buffer = Vec::new();
            buffer.push(0x01);
            buffer.push(0x01);
            buffer.extend_from_slice(pk.as_bytes());
            buffer.extend_from_slice(&sig.to_bytes());
            let metadata = b"{}";
            buffer.extend_from_slice(&(metadata.len() as u16).to_be_bytes());
            buffer.extend_from_slice(metadata);
            buffer
        };

        let sig_bytes = create_sig_bytes(&malicious_verifying_key, &signature);

        let verifier = ManifestVerifier::new(Some(trusted_pubkey_fingerprint), true);

        // Should fail because signer is not trusted
        let res = verifier.verify_manifest(&manifest, &sig_bytes, "");
        assert!(res.is_err());
        assert!(res
            .unwrap_err()
            .to_string()
            .contains("is not the trusted signer"));
    }

    #[test]
    fn test_verifier_rejects_tampered_manifest() {
        // 1. Generate trusted keypair
        let mut csprng = OsRng;
        let trusted_signing_key = SigningKey::generate(&mut csprng);
        let trusted_verifying_key = trusted_signing_key.verifying_key();
        let trusted_pubkey_fingerprint = format!(
            "ed25519:{}",
            BASE64.encode(trusted_verifying_key.as_bytes())
        );

        // 2. Create and sign original manifest
        let original_manifest = create_test_manifest("original-capsule");
        let canonical_bytes = manifest_to_capnp_bytes(&original_manifest)
            .expect("Failed to generate canonical bytes");
        let signature = trusted_signing_key.sign(&canonical_bytes);

        let create_sig_bytes = |pk: &VerifyingKey, sig: &ed25519_dalek::Signature| -> Vec<u8> {
            let mut buffer = Vec::new();
            buffer.push(0x01);
            buffer.push(0x01);
            buffer.extend_from_slice(pk.as_bytes());
            buffer.extend_from_slice(&sig.to_bytes());
            let metadata = b"{}";
            buffer.extend_from_slice(&(metadata.len() as u16).to_be_bytes());
            buffer.extend_from_slice(metadata);
            buffer
        };

        let sig_bytes = create_sig_bytes(&trusted_verifying_key, &signature);

        let verifier = ManifestVerifier::new(Some(trusted_pubkey_fingerprint), true);

        // 3. Create a TAMPERED manifest (different name)
        let tampered_manifest = create_test_manifest("hacked-capsule");

        // Should fail because content was tampered
        let res = verifier.verify_manifest(&tampered_manifest, &sig_bytes, "");
        assert!(res.is_err());
        assert!(res
            .unwrap_err()
            .to_string()
            .contains("Cryptographic verification failed"));
    }

    #[test]
    fn test_json_and_capnp_inputs_produce_same_verification() {
        // This is the key test for canonical signing:
        // Both JSON and Cap'n Proto inputs should produce the same verification result

        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let verifying_key = signing_key.verifying_key();
        let pubkey_fingerprint = format!("ed25519:{}", BASE64.encode(verifying_key.as_bytes()));

        let manifest = create_test_manifest("format-agnostic");

        // Sign using CANONICAL bytes
        let canonical_bytes =
            manifest_to_capnp_bytes(&manifest).expect("Failed to generate canonical bytes");
        let signature = signing_key.sign(&canonical_bytes);

        let create_sig_bytes = |pk: &VerifyingKey, sig: &ed25519_dalek::Signature| -> Vec<u8> {
            let mut buffer = Vec::new();
            buffer.push(0x01);
            buffer.push(0x01);
            buffer.extend_from_slice(pk.as_bytes());
            buffer.extend_from_slice(&sig.to_bytes());
            let metadata = b"{}";
            buffer.extend_from_slice(&(metadata.len() as u16).to_be_bytes());
            buffer.extend_from_slice(metadata);
            buffer
        };

        let sig_bytes = create_sig_bytes(&verifying_key, &signature);

        let verifier = ManifestVerifier::new(Some(pubkey_fingerprint), true);

        // Verify via JSON input
        let json_content = serde_json::to_vec(&manifest).unwrap();
        let json_result = verifier.verify(&json_content, &sig_bytes, "");

        // Verify via struct (which uses canonical bytes internally)
        let struct_result = verifier.verify_manifest(&manifest, &sig_bytes, "");

        // Both should succeed
        assert!(json_result.is_ok(), "JSON verification should pass");
        assert!(struct_result.is_ok(), "Struct verification should pass");
    }
}
