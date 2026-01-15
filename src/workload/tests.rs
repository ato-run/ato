#[cfg(test)]
mod manifest_tests {
    use crate::workload::manifest_loader::load_manifest_str;

    #[test]
    fn parse_json_legacy() {
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
            runtime = "source"
            entrypoint = "main.py"
        "#;
        let (m, r) = load_manifest_str(None, toml).unwrap();
        assert_eq!(m.name, "meeting-summarizer");

        let req = r.unwrap();
        // V1 parser extracts VRAM requirement
        assert!(req.gpu_memory_bytes.is_some());
        assert_eq!(req.gpu_memory_bytes, Some(8 * 1024 * 1024 * 1024));
    }

    #[test]
    fn parse_toml_services() {
        let toml = r#"
            schema_version = "1.0"
            name = "my-ai-stack"
            version = "0.1.0"
            type = "app"

            [execution]
            runtime = "source"
            entrypoint = "noop"

            [services.llm]
            entrypoint = "python server.py --port {{PORT}}"

            [services.web]
            entrypoint = "node ui.js"
            depends_on = ["llm"]
        "#;

        let (m, _r) = load_manifest_str(None, toml).unwrap();
        let services = m.services.expect("services should be parsed");
        assert!(services.contains_key("llm"));
        assert!(services.contains_key("web"));
        assert_eq!(
            services.get("web").unwrap().depends_on.clone().unwrap(),
            vec!["llm".to_string()]
        );
    }
}
