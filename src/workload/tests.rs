#[cfg(test)]
mod manifest_tests {
    use crate::workload::manifest_loader::load_manifest_str;

    #[test]
    fn parse_json_adep() {
        let json = r#"{
            "schema_version": "1.0",
            "name": "test-app",
            "version": "0.1.0",
            "type": "app",
            "execution": {
                "runtime": "docker",
                "entrypoint": "image:tag"
            }
        }"#;
        let (m, r) = load_manifest_str(None, json).unwrap();
        assert_eq!(m.name, "test-app");
        // Requirements are empty by default
        let req = r.unwrap();
        assert!(req.cpu_cores.is_none());
        assert!(req.memory_bytes.is_none());
    }

    #[cfg(feature = "toml-support")]
    #[test]
    fn parse_toml_capsule() {
        let toml = r#"
            schema_version = "1.0"
            name = "meeting-summarizer"
            version = "0.1.0"
            type = "inference"
            
            [metadata]
            display_name = "Meeting Summarizer"

            [capabilities]
            chat = true
            
            [requirements]
            vram_min = "8GB"

            [model]
            source = "hf:foo/bar"

            [execution]
            runtime = "python-uv"
            entrypoint = "main.py"
        "#;
        let (m, r) = load_manifest_str(None, toml).unwrap();
        assert_eq!(m.name, "meeting-summarizer");

        let req = r.unwrap();
        // V1 parser extracts VRAM requirement
        assert!(req.gpu_memory_bytes.is_some());
        assert_eq!(req.gpu_memory_bytes, Some(8 * 1024 * 1024 * 1024));
    }
}
