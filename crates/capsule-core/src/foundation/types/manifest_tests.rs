use std::fs;

use super::{
    is_kebab_case, is_semver, CapsuleManifest, CapsuleType, ConfigField, ConfigKind, RouteWeight,
    RuntimeType, ValidationError, ValidationMode,
};

const VALID_TOML: &str = r#"
schema_version = "0.3"
name = "mlx-qwen3-8b"
version = "1.0.0"
type = "inference"

runtime = "source"
port = 8081
health_check = "/health"
startup_timeout = 120
GUMBALL_MODEL = "qwen3-8b"
run = "server.py"
[env]
[metadata]
display_name = "Qwen3 8B (MLX)"
description = "Local inference on Apple Silicon"
author = "gumball-official"
tags = ["llm", "mlx"]

[capabilities]
chat = true
function_calling = true
vision = false
context_length = 128000

[requirements]
platform = ["darwin-arm64"]
vram_min = "6GB"
vram_recommended = "8GB"
disk = "5GB"

[routing]
weight = "light"
fallback_to_cloud = true
cloud_capsule = "vllm-qwen3-8b"

[model]
source = "hf:org/model"
quantization = "4bit"
"#;

#[test]
fn test_parse_valid_toml() {
    let manifest = CapsuleManifest::from_toml(VALID_TOML).unwrap();

    assert_eq!(manifest.name, "mlx-qwen3-8b");
    assert_eq!(manifest.version, "1.0.0");
    assert_eq!(manifest.capsule_type, CapsuleType::Inference);
    assert_eq!(manifest.resolve_default_target().unwrap().port, Some(8081));
    assert_eq!(
        manifest.resolve_default_runtime().unwrap(),
        RuntimeType::Source
    );
    assert!(manifest.capabilities.as_ref().unwrap().chat);
    assert_eq!(manifest.routing.weight, RouteWeight::Light);
}

#[test]
fn test_validate_valid_manifest() {
    let manifest = CapsuleManifest::from_toml(VALID_TOML).unwrap();
    assert!(manifest.validate().is_ok());
}

#[test]
fn test_parse_job_manifest_type() {
    let manifest = CapsuleManifest::from_toml(
        r#"
schema_version = "0.3"
name = "job-demo"
version = "0.1.0"
type = "job"

runtime = "source/python"
run = "main.py""#,
    )
    .unwrap();

    assert_eq!(manifest.capsule_type, CapsuleType::Job);
}

#[test]
fn test_validate_job_manifest_rejects_ports() {
    let manifest = CapsuleManifest::from_toml(
        r#"
schema_version = "0.3"
name = "job-demo"
version = "0.1.0"
type = "job"

runtime = "source/python"
port = 8080
run = "main.py""#,
    )
    .unwrap();

    let errors = manifest
        .validate()
        .expect_err("job manifest must reject ports");
    assert!(errors.iter().any(|error| {
        matches!(
            error,
            ValidationError::InvalidTarget(message)
                if message.contains("target 'app' declares port")
        )
    }));
}

#[test]
fn test_validate_job_manifest_rejects_web_runtime() {
    let manifest = CapsuleManifest::from_toml(
        r#"
schema_version = "0.3"
name = "job-web-demo"
version = "0.1.0"
type = "job"

runtime = "web/static"
port = 8080
run = "index.html""#,
    )
    .unwrap();

    let errors = manifest
        .validate()
        .expect_err("job manifest must reject runtime=web");
    assert!(errors.iter().any(|error| {
        matches!(
            error,
            ValidationError::InvalidTarget(message)
                if message.contains("target 'app' uses runtime=web")
        )
    }));
}

#[test]
fn test_validate_invalid_schema_version() {
    let toml = VALID_TOML.replace("schema_version = \"0.3\"", "schema_version = \"2.0\"");
    let manifest = CapsuleManifest::from_toml(&toml).unwrap();
    let errors = manifest.validate().unwrap_err();
    assert!(errors
        .iter()
        .any(|e| matches!(e, ValidationError::InvalidSchemaVersion(_))));
}

#[test]
fn test_from_toml_accepts_v03_single_package_manifest() {
    let toml = r#"
schema_version = "0.3"
name = "v03-demo"
version = "0.1.0"
type = "app"
runtime = "source/node"
build = "npm run build"
run = "npm start"
port = 3000
required_env = ["DATABASE_URL"]
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse v0.3 manifest");
    assert_eq!(manifest.schema_version, "0.3");
    assert_eq!(manifest.default_target, "app");

    let target = manifest.resolve_default_target().expect("default target");
    assert_eq!(target.runtime, "source");
    assert_eq!(target.driver.as_deref(), Some("node"));
    assert!(target.entrypoint.is_empty());
    assert!(target.cmd.is_empty());
    assert_eq!(target.run_command.as_deref(), Some("npm start"));
    assert_eq!(target.port, Some(3000));
    assert_eq!(target.required_env, vec!["DATABASE_URL".to_string()]);
    assert_eq!(
        manifest
            .build
            .as_ref()
            .and_then(|build| build.lifecycle.as_ref())
            .and_then(|lifecycle| lifecycle.build.as_deref()),
        Some("npm run build")
    );
}

#[test]
fn test_from_toml_accepts_v03_legacy_env_required_compatibility() {
    let toml = r#"
schema_version = "0.3"
name = "v03-demo"
version = "0.1.0"
type = "app"
runtime = "source/python"
run = "uv run main.py"
required_env = ["DATABASE_URL"]

[env]
required = ["REDIS_URL"]
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse v0.3 manifest");
    let target = manifest.resolve_default_target().expect("default target");

    assert_eq!(
        target.required_env,
        vec!["DATABASE_URL".to_string(), "REDIS_URL".to_string()]
    );
}

#[test]
fn test_from_toml_accepts_chml_single_package_manifest() {
    let toml = r#"
name = "chml-demo"
type = "app"
runtime = "source/node"
build = "npm run build"
outputs = ["dist/**"]
build_env = ["NODE_ENV", "API_BASE_URL"]
run = "npm start"
port = 3000
required_env = ["DATABASE_URL"]

[external_injection]
MODEL_DIR = "directory"
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse CHML manifest");
    assert_eq!(manifest.schema_version, "0.3");
    assert!(manifest.version.is_empty());
    assert_eq!(manifest.default_target, "app");
    assert!(manifest.validate().is_ok());

    let rendered = manifest.to_toml().expect("serialize manifest");
    let rendered_value: toml::Value = toml::from_str(&rendered).expect("parse rendered toml");
    assert!(rendered_value.get("version").is_none());

    let target = manifest.resolve_default_target().expect("default target");
    assert_eq!(target.runtime, "source");
    assert_eq!(target.driver.as_deref(), Some("node"));
    assert_eq!(target.run_command.as_deref(), Some("npm start"));
    assert_eq!(target.outputs, vec!["dist/**".to_string()]);
    assert_eq!(
        target.build_env,
        vec!["NODE_ENV".to_string(), "API_BASE_URL".to_string()]
    );
    assert_eq!(
        target.external_injection["MODEL_DIR"].injection_type,
        "directory"
    );
}

#[test]
fn test_from_toml_preserves_v03_run_command_without_splitting() {
    let toml = r#"
schema_version = "0.3"
name = "json-server"
version = "0.1.0"
type = "app"
runtime = "source/node"
run = "node src/bin.ts fixtures/db.json"
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse v0.3 manifest");
    let target = manifest.resolve_default_target().expect("default target");

    assert!(target.entrypoint.is_empty());
    assert_eq!(target.driver.as_deref(), Some("node"));
    assert_eq!(target.language.as_deref(), Some("node"));
    assert_eq!(
        target.run_command.as_deref(),
        Some("node src/bin.ts fixtures/db.json")
    );
    assert!(target.cmd.is_empty());
}

#[test]
fn test_from_toml_preserves_v03_readiness_probe_table() {
    let toml = r#"
schema_version = "0.3"
name = "probe-demo"
version = "0.1.0"
type = "app"
runtime = "source/node"
run = "npm start -- --port $PORT"
port = 3000
readiness_probe = { http_get = "/healthz", port = "PORT" }
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse v0.3 manifest");
    let target = manifest.resolve_default_target().expect("default target");

    assert_eq!(
        target.run_command.as_deref(),
        Some("npm start -- --port $PORT")
    );
    assert_eq!(
        target
            .readiness_probe
            .as_ref()
            .and_then(|probe| probe.http_get.as_deref()),
        Some("/healthz")
    );
    assert_eq!(
        target
            .readiness_probe
            .as_ref()
            .map(|probe| probe.port.as_str()),
        Some("PORT")
    );
}

#[test]
fn test_from_toml_parses_dependency_contract_manifest_blocks() {
    let toml = r#"
schema_version = "0.3"
name = "consumer-app"
version = "0.1.0"
type = "app"
runtime = "source/python"
run = "uv run app.py"

[dependencies.db]
capsule = "capsule://ato/postgres@16"
contract = "service@1"

  [dependencies.db.parameters]
  database = "appdb"
  pool_size = 8

  [dependencies.db.credentials]
  password = "{{env.PG_PASSWORD}}"

  [dependencies.db.state]
  name = "primary"
  ownership = "parent"
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse dependency contract manifest");
    let dependency = manifest.dependencies.get("db").expect("db dependency");

    assert_eq!(dependency.capsule.0, "capsule://ato/postgres@16");
    assert_eq!(dependency.contract.to_string(), "service@1");
    assert_eq!(
        dependency.parameters.get("database"),
        Some(&super::ParamValue::String("appdb".to_string()))
    );
    assert_eq!(
        dependency.parameters.get("pool_size"),
        Some(&super::ParamValue::Int(8))
    );
    assert_eq!(
        dependency
            .credentials
            .get("password")
            .map(ToString::to_string)
            .as_deref(),
        Some("{{env.PG_PASSWORD}}")
    );
    assert_eq!(
        dependency.state.as_ref().map(|state| state.name.as_str()),
        Some("primary")
    );
}

#[test]
fn test_from_toml_round_trips_provider_contract_manifest_blocks() {
    let toml = r#"
schema_version = "0.3"
name = "postgres-provider"
version = "0.1.0"
type = "app"
default_target = "server"

[targets.server]
runtime = "source"
driver = "native"
run = "postgres -D {{state.dir}} -p {{port}}"
port = 5432

[contracts."service@1"]
target = "server"
ready = { type = "probe", run = "pg_isready -h {{host}} -p {{port}}", timeout = "15s" }

  [contracts."service@1".parameters.database]
  type = "string"
  required = true

  [contracts."service@1".credentials.password]
  type = "string"
  required = true

  [contracts."service@1".identity_exports]
  service_name = "{{params.database}}"

  [contracts."service@1".runtime_exports]
  PGHOST = "{{host}}"

  [contracts."service@1".runtime_exports.DATABASE_URL]
  value = "postgresql://postgres:{{credentials.password}}@{{host}}:{{port}}/{{params.database}}"
  secret = true

  [contracts."service@1".state]
  required = true
  version = "16"
  mount = "/var/lib/postgresql/data"
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse provider contract manifest");
    let contract = manifest
        .contracts
        .get("service@1")
        .expect("service contract");

    assert_eq!(contract.target, "server");
    assert_eq!(
        contract.identity_exports["service_name"].to_string(),
        "{{params.database}}"
    );
    assert_eq!(
        contract.runtime_exports["PGHOST"],
        super::RuntimeExportSpec::Shorthand(super::TemplatedString {
            segments: vec![super::TemplateSegment::Expr(super::TemplateExpr::Host)],
        })
    );
    assert_eq!(
        contract
            .state
            .as_ref()
            .and_then(|state| state.mount.as_deref()),
        Some("/var/lib/postgresql/data")
    );

    let rendered = manifest
        .to_toml()
        .expect("serialize provider contract manifest");
    let reparsed =
        CapsuleManifest::from_toml(&rendered).expect("reparse serialized provider contract");
    assert_eq!(
        reparsed.contracts["service@1"].identity_exports["service_name"].to_string(),
        "{{params.database}}"
    );
    assert_eq!(
        reparsed.contracts["service@1"].runtime_exports["PGHOST"],
        contract.runtime_exports["PGHOST"]
    );
}

#[test]
fn test_from_toml_rejects_unknown_contract_template_expression() {
    let toml = r#"
schema_version = "0.3"
name = "bad-contract"
version = "0.1.0"
type = "app"
runtime = "source/python"
run = "uv run app.py"

[dependencies.db]
capsule = "capsule://ato/postgres@16"
contract = "service@1"

  [dependencies.db.credentials]
  password = "{{unknown.token}}"
"#;

    let error = CapsuleManifest::from_toml(toml).expect_err("unknown template must fail");
    assert!(error
        .to_string()
        .contains("unsupported template expression"));
}

#[test]
fn test_validate_dependency_contracts_reject_missing_contract_target() {
    let toml = r#"
schema_version = "0.3"
name = "postgres-provider"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "postgres -D ./data"

[contracts."service@1"]
target = "missing"
ready = { type = "probe", run = "pg_isready -h {{host}} -p {{port}}" }
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse contract manifest");
    let errors = manifest
        .validate()
        .expect_err("missing contract target must fail");
    assert!(errors.iter().any(|error| {
        matches!(
            error,
            ValidationError::InvalidTarget(message)
                if message.contains("contracts.service@1 references missing target 'missing'")
        )
    }));
}

#[test]
fn test_validate_dependency_contracts_reject_export_collisions() {
    let toml = r#"
schema_version = "0.3"
name = "postgres-provider"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "postgres -D ./data"

[contracts."service@1"]
target = "app"
ready = { type = "probe", run = "pg_isready -h {{host}} -p {{port}}" }

  [contracts."service@1".identity_exports]
  host = "{{host}}"

  [contracts."service@1".runtime_exports]
  host = "{{host}}"
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse contract manifest");
    let errors = manifest.validate().expect_err("export collision must fail");
    assert!(errors.iter().any(|error| {
        matches!(
            error,
            ValidationError::InvalidTarget(message)
                if message.contains("cannot be declared in both identity_exports and runtime_exports")
        )
    }));
}

#[test]
fn test_validate_dependency_contracts_reject_credential_defaults() {
    let toml = r#"
schema_version = "0.3"
name = "postgres-provider"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "postgres -D ./data"

[contracts."service@1"]
target = "app"
ready = { type = "probe", run = "pg_isready -h {{host}} -p {{port}}" }

  [contracts."service@1".credentials.password]
  type = "string"
  required = true
  default = "secret"
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse contract manifest");
    let errors = manifest
        .validate()
        .expect_err("credential defaults must fail validation");
    assert!(errors.iter().any(|error| {
        matches!(
            error,
            ValidationError::InvalidTarget(message)
                if message.contains("contracts.service@1.credentials.password must not declare a default")
        )
    }));
}

#[test]
fn test_validate_v03_library_without_run_is_ok() {
    let toml = r#"
schema_version = "0.3"
name = "shared-ui"
version = "0.1.0"
type = "library"
build = "npm run build"
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse v0.3 library");
    assert_eq!(manifest.capsule_type, CapsuleType::Library);
    assert!(manifest.validate().is_ok());
}

#[test]
fn test_validate_v03_library_rejects_run_command() {
    let toml = r#"
schema_version = "0.3"
name = "shared-ui"
version = "0.1.0"
type = "library"
runtime = "source/node"
run = "npm start"
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse v0.3 library");
    let errors = manifest.validate().expect_err("library run must fail");
    assert!(errors.iter().any(|error| {
        matches!(error, ValidationError::InvalidTarget(message) if message.contains("must not define a run command"))
    }));
}

#[test]
fn test_from_toml_accepts_v03_workspace_packages_as_named_targets() {
    let toml = r#"
schema_version = "0.3"
name = "workspace-demo"

[workspace]
members = ["apps/*"]

[workspace.defaults]
runtime = "source/node"
required_env = ["DATABASE_URL"]

[packages.web]
type = "app"
build = "pnpm --filter web build"
run = "pnpm --filter web start"
port = 3000

    [packages.web.dependencies]
    ui = "workspace:ui"

[packages.ui]
type = "library"
build = "pnpm --filter ui build"
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse v0.3 workspace");
    assert_eq!(manifest.default_target, "web");
    assert!(manifest.version.is_empty());

    let web = manifest
        .targets
        .as_ref()
        .and_then(|targets| targets.named_target("web"))
        .expect("web target");
    assert_eq!(web.package_type.as_deref(), Some("app"));
    assert_eq!(
        web.build_command.as_deref(),
        Some("pnpm --filter web build")
    );
    assert_eq!(web.run_command.as_deref(), Some("pnpm --filter web start"));
    assert_eq!(web.required_env, vec!["DATABASE_URL".to_string()]);
    assert_eq!(web.package_dependencies, vec!["ui".to_string()]);

    let ui = manifest
        .targets
        .as_ref()
        .and_then(|targets| targets.named_target("ui"))
        .expect("ui target");
    assert_eq!(ui.package_type.as_deref(), Some("library"));
    assert_eq!(ui.build_command.as_deref(), Some("pnpm --filter ui build"));
}

#[test]
fn test_from_toml_preserves_workspace_setup_surface() {
    let toml = r#"
schema_version = "0.3"
name = "desky"
version = "0.1.0"
type = "app"
default_target = "desktop"

[targets.desktop]
runtime = "source"
driver = "native"
run = "open Desky.app"

[workspace]
default_app = "desky"

[workspace.apps.desky]
source = "ato/desky"

[workspace.apps.desky.personalization]
model_tier = "balanced"
privacy_mode = "strict"

[workspace.tools.opencode]
source = "ato/opencode-engine"
version = "0.4.0"

[workspace.services.ollama]
source = "ato/ollama-runtime"
mode = "reuse-if-present"
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse workspace setup manifest");
    let workspace = manifest.workspace.as_ref().expect("workspace setup");
    assert_eq!(workspace.default_app.as_deref(), Some("desky"));
    assert_eq!(workspace.apps["desky"].dependency.source, "ato/desky");
    assert_eq!(
        workspace.apps["desky"]
            .personalization
            .as_ref()
            .and_then(|value| value.model_tier.as_deref()),
        Some("balanced")
    );
    assert_eq!(
        workspace.tools["opencode"].version.as_deref(),
        Some("0.4.0")
    );
    assert_eq!(
        workspace.services["ollama"].mode.as_deref(),
        Some("reuse-if-present")
    );
}

#[test]
fn test_from_toml_accepts_chml_workspace_packages_as_named_targets() {
    let toml = r#"
name = "workspace-demo"

[workspace]
members = ["apps/*"]

[workspace.defaults]
runtime = "source/node"
required_env = ["DATABASE_URL"]

[packages.web]
type = "app"
build = "pnpm --filter web build"
outputs = ["apps/web/dist/**"]
build_env = ["NODE_ENV"]
run = "pnpm --filter web start"
port = 3000

    [packages.web.dependencies]
    ui = "workspace:ui"

[packages.ui]
type = "library"
build = "pnpm --filter ui build"
outputs = ["packages/ui/dist/**"]
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse CHML workspace");
    assert_eq!(manifest.schema_version, "0.3");
    assert!(manifest.version.is_empty());
    assert_eq!(manifest.default_target, "web");
    assert!(manifest.validate().is_ok());

    let web = manifest
        .targets
        .as_ref()
        .and_then(|targets| targets.named_target("web"))
        .expect("web target");
    assert_eq!(web.outputs, vec!["apps/web/dist/**".to_string()]);
    assert_eq!(web.build_env, vec!["NODE_ENV".to_string()]);
    assert_eq!(web.required_env, vec!["DATABASE_URL".to_string()]);

    let ui = manifest
        .targets
        .as_ref()
        .and_then(|targets| targets.named_target("ui"))
        .expect("ui target");
    assert_eq!(ui.outputs, vec!["packages/ui/dist/**".to_string()]);
}

#[test]
fn test_validate_v03_workspace_rejects_dependency_cycles() {
    let toml = r#"
schema_version = "0.3"
name = "workspace-demo"

[packages.web]
type = "app"
runtime = "source/node"
run = "pnpm --filter web start"

  [packages.web.dependencies]
  ui = "workspace:ui"

[packages.ui]
type = "library"
runtime = "source/node"
build = "pnpm --filter ui build"

  [packages.ui.dependencies]
  web = "workspace:web"
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse v0.3 workspace");
    let errors = manifest.validate().expect_err("cycle must fail");
    assert!(errors.iter().any(|error| {
        matches!(error, ValidationError::InvalidTarget(message) if message.contains("circular dependency detected"))
    }));
}

#[test]
fn test_from_toml_rejects_v03_top_level_legacy_entrypoint() {
    let toml = r#"
schema_version = "0.3"
name = "legacy-v03"
version = "0.1.0"
type = "app"
runtime = "source/node"
entrypoint = "server.js"
"#;

    let error = CapsuleManifest::from_toml(toml).expect_err("v0.3 entrypoint must fail");
    assert!(error
        .to_string()
        .contains("must not use legacy field 'entrypoint'"));
}

#[test]
fn test_from_toml_rejects_v03_target_legacy_cmd() {
    let toml = r#"
schema_version = "0.3"
name = "legacy-v03"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"
cmd = ["python", "app.py"]
"#;

    let error = CapsuleManifest::from_toml(toml).expect_err("v0.3 cmd must fail");
    assert!(error
        .to_string()
        .contains("must not use legacy field 'cmd'"));
}

#[test]
fn test_load_from_file_supports_v03_capsule_path_single_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root_manifest = tmp.path().join("capsule.toml");
    let package_dir = tmp.path().join("apps").join("api");
    fs::create_dir_all(&package_dir).expect("create package dir");

    fs::write(
        &root_manifest,
        r#"
schema_version = "0.3"
name = "workspace-demo"

[packages.api]
capsule_path = "./apps/api"
"#,
    )
    .expect("write root manifest");

    fs::write(
        package_dir.join("capsule.toml"),
        r#"
schema_version = "0.3"
name = "api"
type = "app"
runtime = "source/node"
run = "pnpm start"
"#,
    )
    .expect("write delegated manifest");

    let manifest = CapsuleManifest::load_from_file(&root_manifest).expect("load manifest");
    let api = manifest
        .targets
        .as_ref()
        .and_then(|targets| targets.named_target("api"))
        .expect("api target");

    assert_eq!(manifest.default_target, "api");
    assert_eq!(api.run_command.as_deref(), Some("pnpm start"));
    assert_eq!(api.working_dir.as_deref(), Some("apps/api"));
}

#[test]
fn test_load_from_file_ignores_generated_workspace_member_dirs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root_manifest = tmp.path().join("capsule.toml");
    let control_plane_dir = tmp.path().join("apps").join("control-plane");
    let dashboard_dir = tmp.path().join("apps").join("dashboard");
    let generated_duplicate_dir = dashboard_dir
        .join(".next")
        .join("standalone")
        .join("apps")
        .join("control-plane");
    fs::create_dir_all(&control_plane_dir).expect("create control-plane dir");
    fs::create_dir_all(&dashboard_dir).expect("create dashboard dir");
    fs::create_dir_all(&generated_duplicate_dir).expect("create generated duplicate dir");

    fs::write(
        &root_manifest,
        r#"
name = "file2api"

[workspace]
members = ["apps/*"]

[packages.control-plane]
type = "app"
runtime = "source/python"
run = "uvicorn control_plane.modal_webhook:app --host 0.0.0.0 --port $PORT"
port = 8000

[packages.dashboard]
type = "app"
runtime = "source/node"
build = "npm run build"
run = "npm start"
port = 3000
"#,
    )
    .expect("write root manifest");

    fs::write(
        control_plane_dir.join("capsule.toml"),
        "name = \"control-plane\"\ntype = \"app\"\nruntime = \"source/python\"\nrun = \"python main.py\"\n",
    )
    .expect("write control-plane manifest");
    fs::write(
        dashboard_dir.join("capsule.toml"),
        "name = \"dashboard\"\ntype = \"app\"\nruntime = \"source/node\"\nrun = \"npm start\"\n",
    )
    .expect("write dashboard manifest");
    fs::write(
        generated_duplicate_dir.join("capsule.toml"),
        "name = \"control-plane\"\ntype = \"app\"\nruntime = \"source/python\"\nrun = \"python generated.py\"\n",
    )
    .expect("write generated duplicate manifest");

    let manifest = CapsuleManifest::load_from_file(&root_manifest).expect("load manifest");
    let targets = manifest.targets.expect("targets");
    assert!(targets.named_target("control-plane").is_some());
    assert!(targets.named_target("dashboard").is_some());
}

#[test]
fn test_validate_cli_export_python_tool_target_is_ok() {
    let toml = r#"
schema_version = "0.3"
name = "python-tool-demo"
version = "0.1.0"
type = "app"

runtime = "source/python"
runtime_version = "3.11"
run = "main.py"
[exports.cli.demo-tool]
kind = "python-tool"
target = "app"
args = ["--mode", "oneshot"]
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse export manifest");
    assert!(manifest.validate().is_ok());
}

#[test]
fn test_validate_cli_export_rejects_missing_target() {
    let toml = r#"
schema_version = "0.3"
name = "python-tool-demo"
version = "0.1.0"
type = "app"

runtime = "source/python"
runtime_version = "3.11"
run = "main.py"
[exports.cli.demo-tool]
kind = "python-tool"
target = "missing"
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse export manifest");
    let errors = manifest
        .validate()
        .expect_err("missing export target must fail");
    assert!(errors.iter().any(|error| {
        matches!(error, ValidationError::InvalidTarget(message) if message.contains("references missing target 'missing'"))
    }));
}

#[test]
fn test_validate_cli_export_rejects_non_python_target() {
    let toml = r#"
schema_version = "0.3"
name = "node-tool-demo"
version = "0.1.0"
type = "app"

runtime = "source/node"
runtime_version = "20"
run = "main.js"
[exports.cli.demo-tool]
kind = "python-tool"
target = "app"
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse export manifest");
    let errors = manifest
        .validate()
        .expect_err("non-python export target must fail");
    assert!(errors.iter().any(|error| {
        matches!(error, ValidationError::InvalidTarget(message) if message.contains("must reference a source/python target"))
    }));
}

#[test]
fn test_load_from_file_supports_v03_capsule_path_workspace_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root_manifest = tmp.path().join("capsule.toml");
    let delegated_dir = tmp.path().join("packages").join("shared");
    fs::create_dir_all(&delegated_dir).expect("create delegated dir");

    fs::write(
        &root_manifest,
        r#"
schema_version = "0.3"
name = "workspace-demo"

[packages.shared]
capsule_path = "./packages/shared"
"#,
    )
    .expect("write root manifest");

    fs::write(
        delegated_dir.join("capsule.toml"),
        r#"
schema_version = "0.3"
name = "shared-workspace"

[packages.ui]
type = "library"
runtime = "source/node"
build = "pnpm --filter ui build"

[packages.web]
type = "app"
runtime = "source/node"
run = "pnpm --filter web start"

  [packages.web.dependencies]
  ui = "workspace:ui"
"#,
    )
    .expect("write delegated workspace manifest");

    let manifest = CapsuleManifest::load_from_file(&root_manifest).expect("load manifest");
    let targets = manifest.targets.as_ref().expect("targets");
    let ui = targets.named_target("ui").expect("ui target");
    let web = targets.named_target("web").expect("web target");

    assert_eq!(manifest.default_target, "web");
    assert_eq!(ui.working_dir.as_deref(), Some("packages/shared"));
    assert_eq!(web.working_dir.as_deref(), Some("packages/shared"));
    assert_eq!(web.package_dependencies, vec!["ui".to_string()]);
}

#[test]
fn test_load_from_file_expands_workspace_members_and_resolves_workspace_path_dependencies() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root_manifest = tmp.path().join("capsule.toml");
    let web_dir = tmp.path().join("apps").join("web");
    let ui_dir = tmp.path().join("packages").join("ui");
    let api_dir = tmp.path().join("apps").join("api_gateway");
    fs::create_dir_all(&web_dir).expect("create web dir");
    fs::create_dir_all(&ui_dir).expect("create ui dir");
    fs::create_dir_all(&api_dir).expect("create api dir");

    fs::write(web_dir.join("capsule.toml"), "name = 'web-marker'\n")
        .expect("write web marker manifest");
    fs::write(
        ui_dir.join("capsule.toml"),
        r#"
schema_version = "0.3"
name = "ui"
type = "library"
build = "pnpm --filter ui build"
"#,
    )
    .expect("write ui manifest");
    fs::write(
        api_dir.join("capsule.toml"),
        r#"
schema_version = "0.3"
name = "api-gateway"
type = "app"
runtime = "source/node"
run = "pnpm --filter api start"
"#,
    )
    .expect("write api manifest");
    fs::write(
        &root_manifest,
        r#"
schema_version = "0.3"
name = "workspace-demo"

[workspace]
members = ["apps/*", "packages/*"]

[workspace.defaults]
runtime = "source/node"

[packages.web]
type = "app"
run = "pnpm --filter web start"

  [packages.web.dependencies]
  ui = "workspace:packages/ui"

[packages.api_gateway]
capsule_path = "./apps/api_gateway"
"#,
    )
    .expect("write root manifest");

    let manifest = CapsuleManifest::load_from_file(&root_manifest).expect("load manifest");
    let targets = manifest.targets.as_ref().expect("targets");
    let web = targets.named_target("web").expect("web target");
    let ui = targets.named_target("ui").expect("ui target");
    let api = targets
        .named_target("api_gateway")
        .expect("api_gateway target");

    assert_eq!(web.working_dir.as_deref(), Some("apps/web"));
    assert_eq!(web.package_dependencies, vec!["ui".to_string()]);
    assert_eq!(ui.working_dir.as_deref(), Some("packages/ui"));
    assert_eq!(api.working_dir.as_deref(), Some("apps/api_gateway"));
}

#[test]
fn test_workspace_path_dependency_resolves_to_explicit_package_label() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root_manifest = tmp.path().join("capsule.toml");
    let ui_dir = tmp.path().join("packages").join("ui");
    fs::create_dir_all(&ui_dir).expect("create ui dir");
    fs::write(
        ui_dir.join("capsule.toml"),
        r#"
schema_version = "0.3"
name = "ui"
type = "library"
build = "pnpm --filter ui build"
"#,
    )
    .expect("write ui manifest");
    fs::write(
        &root_manifest,
        r#"
schema_version = "0.3"
name = "workspace-demo"

[workspace]
members = ["packages/*"]

[packages.web]
type = "app"
runtime = "source/node"
run = "pnpm --filter web start"

  [packages.web.dependencies]
  ui = "workspace:packages/ui"

[packages.shared-ui]
capsule_path = "./packages/ui"
"#,
    )
    .expect("write root manifest");

    let manifest = CapsuleManifest::load_from_file(&root_manifest).expect("load manifest");
    let web = manifest
        .targets
        .as_ref()
        .and_then(|targets| targets.named_target("web"))
        .expect("web target");

    assert_eq!(web.package_dependencies, vec!["shared-ui".to_string()]);
}

#[test]
fn test_load_from_file_preserves_external_capsule_dependencies() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("capsule.toml");
    fs::write(
        &manifest_path,
        r#"
schema_version = "0.3"
name = "workspace-demo"

[packages.web]
type = "app"
runtime = "source/node"
run = "node server.js"

  [packages.web.dependencies]
  auth = "capsule://store/acme/auth-svc"
"#,
    )
    .expect("write manifest");

    let manifest = CapsuleManifest::load_from_file(&manifest_path).expect("load manifest");
    let web = manifest
        .targets
        .as_ref()
        .and_then(|targets| targets.named_target("web"))
        .expect("web target");

    assert_eq!(web.external_dependencies.len(), 1);
    assert_eq!(web.external_dependencies[0].alias, "auth");
    assert_eq!(
        web.external_dependencies[0].source,
        "capsule://ato.run/acme/auth-svc"
    );
    assert_eq!(web.external_dependencies[0].source_type, "store");
}

#[test]
fn test_load_from_file_preserves_external_capsule_dependency_query_bindings() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("capsule.toml");
    fs::write(
        &manifest_path,
        r#"
schema_version = "0.3"
name = "workspace-demo"

[packages.web]
type = "app"
runtime = "source/node"
run = "npm start"

  [packages.web.dependencies]
  auth = "capsule://store/acme/auth-svc?MODEL_DIR=https%3A%2F%2Fdata.tld%2Fweights.zip&CONFIG_FILE=file%3A%2F%2F.%2Fconfig.json"
"#,
    )
    .expect("write manifest");

    let manifest = CapsuleManifest::load_from_file(&manifest_path).expect("load manifest");
    let web = manifest
        .targets
        .as_ref()
        .and_then(|targets| targets.named_target("web"))
        .expect("web target");

    assert_eq!(web.external_dependencies.len(), 1);
    assert_eq!(
        web.external_dependencies[0].source,
        "capsule://ato.run/acme/auth-svc"
    );
    assert_eq!(
        web.external_dependencies[0]
            .injection_bindings
            .get("MODEL_DIR")
            .map(String::as_str),
        Some("https://data.tld/weights.zip")
    );
    assert_eq!(
        web.external_dependencies[0]
            .injection_bindings
            .get("CONFIG_FILE")
            .map(String::as_str),
        Some("file://./config.json")
    );
}

#[test]
fn test_load_from_file_preserves_external_injection_contracts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("capsule.toml");
    fs::write(
        &manifest_path,
        r#"
schema_version = "0.3"
name = "workspace-demo"

[packages.worker]
type = "app"
runtime = "source/python"
run = "python main.py --config $CONFIG_FILE"

  [packages.worker.external_injection]
  MODEL_DIR = "directory"
  CONFIG_FILE = { type = "file", required = false, default = "https://example.test/config.json" }
"#,
    )
    .expect("write manifest");

    let manifest = CapsuleManifest::load_from_file(&manifest_path).expect("load manifest");
    let worker = manifest
        .targets
        .as_ref()
        .and_then(|targets| targets.named_target("worker"))
        .expect("worker target");

    assert_eq!(
        worker.external_injection["MODEL_DIR"].injection_type,
        "directory"
    );
    assert!(worker.external_injection["MODEL_DIR"].required);
    assert_eq!(
        worker.external_injection["CONFIG_FILE"].injection_type,
        "file"
    );
    assert!(!worker.external_injection["CONFIG_FILE"].required);
    assert_eq!(
        worker.external_injection["CONFIG_FILE"].default.as_deref(),
        Some("https://example.test/config.json")
    );
}

#[test]
fn test_load_from_file_rejects_invalid_external_injection_key() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("capsule.toml");
    fs::write(
        &manifest_path,
        r#"
schema_version = "0.3"
name = "workspace-demo"

[packages.worker]
type = "app"
runtime = "source/python"
run = "python main.py"

  [packages.worker.external_injection]
  model_dir = "directory"
"#,
    )
    .expect("write manifest");

    let err = CapsuleManifest::load_from_file(&manifest_path).expect_err("must reject");
    assert!(err
        .to_string()
        .contains("external_injection key 'model_dir'"));
}

#[test]
fn test_validate_invalid_memory_string() {
    let toml = VALID_TOML.replace("vram_min = \"6GB\"", "vram_min = \"6XB\"");
    let manifest = CapsuleManifest::from_toml(&toml).unwrap();
    let errs = manifest.validate().unwrap_err();
    assert!(errs
        .iter()
        .any(|e| matches!(e, ValidationError::InvalidMemoryString { .. })));
}

#[test]
fn test_validate_invalid_name() {
    let toml = VALID_TOML.replace("name = \"mlx-qwen3-8b\"", "name = \"Invalid Name!\"");
    let manifest = CapsuleManifest::from_toml(&toml).unwrap();
    let errors = manifest.validate().unwrap_err();
    assert!(errors
        .iter()
        .any(|e| matches!(e, ValidationError::InvalidName(_))));
}

#[test]
fn test_validate_invalid_driver() {
    let toml = r#"
schema_version = "0.3"
name = "test-capsule"
version = "1.0.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "invalid-driver"
run_command = "server.py"
"#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    let errors = manifest.validate().unwrap_err();
    assert!(errors
        .iter()
        .any(|e| matches!(e, ValidationError::InvalidTargetDriver(_, _))));
}

#[test]
fn test_validate_source_driver_requires_runtime_version() {
    // Use schema_version = "0.2" so the runtime_version check fires (gated on !schema_is_v03)
    let toml = r#"
schema_version = "0.2"
name = "test-capsule"
version = "1.0.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "python"
entrypoint = "server.py"
"#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    let errors = manifest.validate().unwrap_err();
    assert!(errors
        .iter()
        .any(|e| matches!(e, ValidationError::MissingRuntimeVersion(_, _))));
}

#[test]
fn test_validate_preview_allows_missing_runtime_version() {
    // In schema_version=0.3, runtime_version is never required (checked only for older schemas).
    // Verify that a source/python target without runtime_version passes in Preview mode.
    let toml = r#"
schema_version = "0.3"
name = "test-capsule"
version = "1.0.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "python"
run_command = "server.py"
"#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    assert!(manifest.validate_for_mode(ValidationMode::Preview).is_ok());
}

#[test]
fn test_validate_web_requires_driver_and_port() {
    let toml = r#"
schema_version = "0.3"
name = "web-app"
version = "0.1.0"
type = "app"

runtime = "web"
run = "dist""#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    let errors = manifest.validate().unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        e,
        ValidationError::InvalidWebTarget(_, msg) if msg.contains("driver is required")
    )));
    assert!(errors.iter().any(|e| matches!(
        e,
        ValidationError::InvalidWebTarget(_, msg) if msg.contains("port is required")
    )));
}

#[test]
fn test_validate_preview_web_still_requires_driver_but_not_port() {
    let toml = r#"
schema_version = "0.3"
name = "web-app"
version = "0.1.0"
type = "app"

runtime = "web"
run = "dist""#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    let errors = manifest
        .validate_for_mode(ValidationMode::Preview)
        .unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        e,
        ValidationError::InvalidWebTarget(_, msg) if msg.contains("driver is required")
    )));
    assert!(!errors.iter().any(|e| matches!(
        e,
        ValidationError::InvalidWebTarget(_, msg) if msg.contains("port is required")
    )));
}

#[test]
fn test_validate_web_rejects_public_and_browser_static() {
    // Use schema_version = "0.2" so entrypoint is valid; the browser_static driver check fires
    // inside the runtime=web validation block.
    let toml = r#"
schema_version = "0.2"
name = "web-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "web"
driver = "browser_static"
port = 8080
entrypoint = "dist"
"#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    let errors = manifest.validate().unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        e,
        ValidationError::InvalidWebTarget(_, msg) if msg.contains("driver 'browser_static' has been removed")
    )));
}

#[test]
fn test_validate_web_static_accepts_port_and_driver() {
    let toml = r#"
schema_version = "0.3"
name = "web-app"
version = "0.1.0"
type = "app"

runtime = "web/static"
port = 8080
run = "dist""#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    assert!(manifest.validate().is_ok());
}

#[test]
fn test_v03_web_static_run_normalizes_to_directory_entrypoint() {
    let toml = r#"
schema_version = "0.3"
name = "hello-capsule"
version = "0.1.0"
type = "app"
runtime = "web/static"
run = "index.html"
port = 18080
"#;

    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    let target = manifest.resolve_default_target().unwrap();

    assert_eq!(target.runtime, "web");
    assert_eq!(target.driver.as_deref(), Some("static"));
    assert_eq!(target.entrypoint, ".");
    assert_eq!(target.port, Some(18080));
}

#[test]
fn test_validate_web_dynamic_rejects_shell_style_entrypoint() {
    // Use schema_version = "0.2" so entrypoint is a valid legacy field; the shell-style
    // entrypoint check fires for runtime=web, driver=node with multi-word entrypoints.
    let toml = r#"
schema_version = "0.2"
name = "web-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "web"
driver = "node"
port = 3000
entrypoint = "npm run start"
"#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    let errors = manifest.validate().unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        e,
        ValidationError::InvalidWebTarget(_, msg) if msg.contains("shell command strings are not allowed")
    )));
}

#[test]
fn test_validate_web_deno_services_allows_empty_target_entrypoint() {
    let toml = r#"
schema_version = "0.3"
name = "web-services-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "web"
driver = "deno"
port = 4173
[services.main]
entrypoint = "node apps/dashboard/server.js"
"#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    assert!(manifest.validate().is_ok());
}

#[test]
fn test_validate_web_deno_services_requires_main_service() {
    let toml = r#"
schema_version = "0.3"
name = "web-services-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "web"
driver = "deno"
port = 4173
[services.api]
entrypoint = "python apps/api/main.py"
"#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    let errors = manifest.validate().unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        e,
        ValidationError::InvalidService(name, msg)
            if name == "main" && msg.contains("services.main is required")
    )));
}

#[test]
fn test_validate_web_deno_services_rejects_unknown_dependency() {
    let toml = r#"
schema_version = "0.3"
name = "web-services-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "web"
driver = "deno"
port = 4173
[services.main]
entrypoint = "node apps/dashboard/server.js"
depends_on = ["api"]
"#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    let errors = manifest.validate().unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        e,
        ValidationError::InvalidService(name, msg)
            if name == "main" && msg.contains("unknown service 'api'")
    )));
}

#[test]
fn test_validate_web_deno_services_rejects_circular_dependencies() {
    let toml = r#"
schema_version = "0.3"
name = "web-services-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "web"
driver = "deno"
port = 4173
[services.main]
entrypoint = "node apps/dashboard/server.js"
depends_on = ["api"]

[services.api]
entrypoint = "python apps/api/main.py"
depends_on = ["main"]
"#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    let errors = manifest.validate().unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        e,
        ValidationError::InvalidService(name, msg)
            if name == "services" && msg.contains("circular dependency detected")
    )));
}

#[test]
fn test_validate_web_deno_services_rejects_invalid_readiness_probe() {
    let toml = r#"
schema_version = "0.3"
name = "web-services-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "web"
driver = "deno"
port = 4173
[services.main]
entrypoint = "node apps/dashboard/server.js"

[services.api]
entrypoint = "python apps/api/main.py"
readiness_probe = { port = "API_PORT" }
"#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    let errors = manifest.validate().unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        e,
        ValidationError::InvalidService(name, msg)
            if name == "api" && msg.contains("http_get or tcp_connect")
    )));
}

#[test]
fn test_validate_web_deno_services_rejects_expose() {
    let toml = r#"
schema_version = "0.3"
name = "web-services-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "web"
driver = "deno"
port = 4173
[services.main]
entrypoint = "node apps/dashboard/server.js"
expose = ["API_PORT"]
"#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    let errors = manifest.validate().unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        e,
        ValidationError::InvalidService(name, msg)
            if name == "main" && msg.contains("expose is not supported")
    )));
}

#[test]
fn test_validate_ephemeral_state_binding_for_oci_service() {
    let toml = r#"
schema_version = "0.3"
name = "stateful-app"
version = "0.1.0"
type = "app"

runtime = "oci"
image = "ghcr.io/example/app:latest"
[state.data]
kind = "filesystem"
durability = "ephemeral"
purpose = "primary-data"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    assert!(manifest.validate().is_ok());
}

#[test]
fn test_validate_rejects_state_binding_for_non_oci_service() {
    let toml = r#"
schema_version = "0.3"
name = "stateful-app"
version = "0.1.0"
type = "app"

runtime = "web/node"
port = 3000
run = "server.js"
[state.data]
kind = "filesystem"
durability = "ephemeral"
purpose = "primary-data"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    let errors = manifest.validate().unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        e,
        ValidationError::InvalidStateBinding(name, msg)
            if name == "main" && msg.contains("only supported for OCI targets")
    )));
}

#[test]
fn test_validate_accepts_persistent_state_with_explicit_attach() {
    let toml = r#"
schema_version = "0.3"
name = "stateful-app"
version = "0.1.0"
type = "app"

runtime = "oci"
image = "ghcr.io/example/app:latest"
[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "primary-data"
attach = "explicit"
schema_id = "vaultwarden/data/v1"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    assert!(manifest.validate().is_ok());
}

#[test]
fn test_validate_rejects_empty_state_owner_scope() {
    let toml = r#"
schema_version = "0.3"
name = "stateful-app"
version = "0.1.0"
type = "app"
state_owner_scope = "   "

runtime = "oci"
image = "ghcr.io/example/app:latest"
[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "primary-data"
attach = "explicit"
schema_id = "vaultwarden/data/v1"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    let errors = manifest.validate().unwrap_err();
    assert!(errors.iter().any(|error| matches!(
        error,
        ValidationError::InvalidState(name, message)
            if name == "state_owner_scope" && message.contains("cannot be empty")
    )));
}

#[test]
fn test_persistent_state_owner_scope_prefers_explicit_field() {
    let toml = r#"
schema_version = "0.3"
name = "stateful-app"
version = "0.1.0"
type = "app"
state_owner_scope = "tenant/acme/prod"

runtime = "oci"
image = "ghcr.io/example/app:latest"
[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "primary-data"
attach = "explicit"
schema_id = "vaultwarden/data/v1"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    assert_eq!(
        manifest.persistent_state_owner_scope().as_deref(),
        Some("tenant/acme/prod")
    );
}

#[test]
fn test_validate_rejects_empty_service_binding_scope() {
    let toml = r#"
schema_version = "0.3"
name = "stateful-app"
version = "0.1.0"
type = "app"
service_binding_scope = "   "

runtime = "oci"
image = "ghcr.io/example/app:latest"
[services.main]
target = "app"
network = { publish = true }
"#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    let errors = manifest.validate().unwrap_err();
    assert!(errors.iter().any(|error| matches!(
        error,
        ValidationError::InvalidService(name, message)
            if name == "service_binding_scope" && message.contains("cannot be empty")
    )));
}

#[test]
fn test_host_service_binding_scope_prefers_explicit_field() {
    let toml = r#"
schema_version = "0.3"
name = "stateful-app"
version = "0.1.0"
type = "app"
service_binding_scope = "tenant/acme/services"

runtime = "oci"
image = "ghcr.io/example/app:latest"
[services.main]
target = "app"
network = { publish = true }
"#;
    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    assert_eq!(
        manifest.host_service_binding_scope().as_deref(),
        Some("tenant/acme/services")
    );
}

#[test]
fn test_to_json_roundtrip() {
    let manifest = CapsuleManifest::from_toml(VALID_TOML).unwrap();
    let json = manifest.to_json().unwrap();
    let manifest2 = CapsuleManifest::from_json(&json).unwrap();

    assert_eq!(manifest.name, manifest2.name);
    assert_eq!(manifest.version, manifest2.version);
}

#[test]
fn test_parse_build_and_isolation_sections() {
    // Use explicit [targets] to avoid flat-v0.3 normalization, which cannot coexist
    // with a structured [build] table (normalization expects build to be a command string).
    let toml = r#"
schema_version = "0.3"
name = "mlx-qwen3-8b"
version = "1.0.0"
type = "inference"
default_target = "app"

[targets.app]
runtime = "source"
run_command = "server.py"

[build]
exclude_libs = ["**/site-packages/torch/**"]
gpu = true

[build.lifecycle]
prepare = "npm ci"
build = "npm run build"
package = "ato pack"

[build.inputs]
lockfiles = ["package-lock.json"]
toolchain = "node:20"

[build.outputs]
capsule = "dist/*.capsule"
sha256 = true
blake3 = true
attestation = true
signature = true

[build.policy]
require_attestation = true
require_did_signature = true

[isolation]
allow_env = ["LD_LIBRARY_PATH", "HF_TOKEN"]
"#
    .to_string();

    let manifest = CapsuleManifest::from_toml(&toml).unwrap();

    let build = manifest.build.as_ref().expect("build section should exist");
    assert!(build.gpu);
    assert_eq!(build.exclude_libs, vec!["**/site-packages/torch/**"]);
    assert_eq!(
        build.lifecycle.as_ref().and_then(|v| v.prepare.as_deref()),
        Some("npm ci")
    );
    assert_eq!(
        build.inputs.as_ref().and_then(|v| v.toolchain.as_deref()),
        Some("node:20")
    );
    assert_eq!(
        build.outputs.as_ref().and_then(|v| v.capsule.as_deref()),
        Some("dist/*.capsule")
    );
    assert_eq!(
        build.policy.as_ref().and_then(|v| v.require_attestation),
        Some(true)
    );

    let isolation = manifest
        .isolation
        .as_ref()
        .expect("isolation section should exist");
    assert_eq!(isolation.allow_env, vec!["LD_LIBRARY_PATH", "HF_TOKEN"]);
}

#[test]
fn test_display_name() {
    let manifest = CapsuleManifest::from_toml(VALID_TOML).unwrap();
    assert_eq!(manifest.display_name(), "Qwen3 8B (MLX)");
}

#[test]
fn test_can_fallback_to_cloud() {
    let manifest = CapsuleManifest::from_toml(VALID_TOML).unwrap();
    assert!(manifest.can_fallback_to_cloud());
}

#[test]
fn test_vram_parsing() {
    let manifest = CapsuleManifest::from_toml(VALID_TOML).unwrap();
    let vram_min = manifest.requirements.vram_min_bytes().unwrap();
    assert_eq!(vram_min, Some(6 * 1024 * 1024 * 1024));
}

#[test]
fn test_rejects_legacy_execution_section_toml() {
    let legacy_manifest = r#"
schema_version = "0.3"
name = "legacy-app"
version = "0.1.0"
type = "app"
default_target = "app"

[execution]
runtime = "source"
entrypoint = "main.py"

[targets.app]
runtime = "source"
run = "main.py""#;

    let error = CapsuleManifest::from_toml(legacy_manifest).unwrap_err();
    assert!(error
        .to_string()
        .contains("legacy [execution] section is not supported in schema_version=0.3"));
}

#[test]
fn test_rejects_legacy_execution_section_json() {
    let legacy_manifest = r#"{
  "schema_version": "0.2",
  "name": "legacy-app",
  "version": "0.1.0",
  "type": "app",
  "default_target": "cli",
  "execution": {
    "runtime": "source",
    "entrypoint": "main.py"
  },
  "targets": {
    "cli": {
      "runtime": "source",
      "entrypoint": "main.py"
    }
  }
}"#;

    let error = CapsuleManifest::from_json(legacy_manifest).unwrap_err();
    assert!(error
        .to_string()
        .contains("legacy [execution] section is not supported in schema_version=0.3"));
}

#[test]
fn test_validate_orchestration_services_target_mode() {
    let toml = r#"
schema_version = "0.3"
name = "multi-runtime-app"
version = "0.1.0"
type = "app"

default_target = "web"

[targets.web]
runtime = "web/node"
port = 3000
run = "server.js"

[targets.db]
runtime = "oci"
image = "mysql:8"
port = 3306
[services.main]
target = "web"
depends_on = ["db"]

[services.db]
target = "db"
network = { allow_from = ["main"] }
"#;

    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    assert!(manifest.validate().is_ok());
}

#[test]
fn test_validate_orchestration_rejects_unknown_target() {
    let toml = r#"
schema_version = "0.3"
name = "multi-runtime-app"
version = "0.1.0"
type = "app"

runtime = "web/node"
port = 3000
run = "server.js"
[services.main]
target = "missing"
"#;

    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    let errors = manifest.validate().unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        e,
        ValidationError::InvalidService(name, msg)
            if name == "main" && msg.contains("target 'missing' does not exist")
    )));
}

#[test]
fn test_validate_orchestration_rejects_target_and_entrypoint_mix() {
    let toml = r#"
schema_version = "0.3"
name = "multi-runtime-app"
version = "0.1.0"
type = "app"

runtime = "web/node"
port = 3000
run = "server.js"
[services.main]
target = "web"
entrypoint = "node server.js"
"#;

    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    let errors = manifest.validate().unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        e,
        ValidationError::InvalidService(name, msg)
            if name == "main" && msg.contains("mutually exclusive")
    )));
}

#[test]
fn test_validate_oci_target_accepts_image_without_entrypoint() {
    let toml = r#"
schema_version = "0.3"
name = "oci-app"
version = "0.1.0"
type = "app"

runtime = "oci"
image = "mysql:8""#;

    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    assert!(manifest.validate().is_ok());
}

#[test]
fn test_validate_orchestration_rejects_unknown_allow_from() {
    let toml = r#"
schema_version = "0.3"
name = "multi-runtime-app"
version = "0.1.0"
type = "app"

default_target = "web"

[targets.web]
runtime = "web/node"
port = 3000
run = "server.js"

[targets.db]
runtime = "oci"
image = "mysql:8"
port = 3306
[services.main]
target = "web"
depends_on = ["db"]

[services.db]
target = "db"
network = { allow_from = ["api"] }
"#;

    let manifest = CapsuleManifest::from_toml(toml).unwrap();
    let errors = manifest.validate().unwrap_err();
    assert!(errors.iter().any(|e| matches!(
        e,
        ValidationError::InvalidService(name, msg)
            if name == "db" && msg.contains("allow_from references unknown service")
    )));
}

#[test]
fn test_is_kebab_case() {
    assert!(is_kebab_case("valid-name"));
    assert!(is_kebab_case("name123"));
    assert!(is_kebab_case("a1"));
    assert!(!is_kebab_case("Invalid"));
    assert!(!is_kebab_case("-invalid"));
    assert!(!is_kebab_case("invalid-"));
    assert!(!is_kebab_case(""));
}

#[test]
fn test_is_semver() {
    assert!(is_semver("1.0.0"));
    assert!(is_semver("0.1.0"));
    assert!(is_semver("1.0.0-alpha"));
    assert!(!is_semver("1.0"));
    assert!(!is_semver("v1.0.0"));
}

// ─── Config schema (Day 1 of schema-driven dynamic config UI) ──────────────

#[test]
fn resolved_config_schema_derives_secret_from_legacy_required_env() {
    // Legacy form: only `required_env = ["FOO"]` is set. The resolver should
    // synthesize a default ConfigField { kind: Secret } per entry so existing
    // capsules automatically feed the desktop dynamic form.
    let toml = r#"
schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "main"

[targets.main]
runtime = "source"
driver = "node"
entrypoint = "server.js"
required_env = ["OPENAI_API_KEY"]
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse");
    let target = manifest.resolve_default_target().expect("default target");

    assert!(target.config_schema.is_empty(), "rich form not used");
    let resolved = target.resolved_config_schema();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].name, "OPENAI_API_KEY");
    assert!(matches!(resolved[0].kind, ConfigKind::Secret));
    assert!(resolved[0].label.is_none());
}

#[test]
fn config_schema_rich_form_parses_all_kinds_and_wins_over_required_env() {
    // Rich form with one of each kind. When config_schema is non-empty it
    // must take precedence over required_env.
    let toml = r#"
schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "main"

[targets.main]
runtime = "source"
driver = "node"
entrypoint = "server.js"
required_env = ["LEGACY_KEY"]

[[targets.main.config_schema]]
name = "OPENAI_API_KEY"
kind = "secret"
label = "OpenAI API Key"
description = "Your OpenAI API key"
placeholder = "sk-..."

[[targets.main.config_schema]]
name = "USERNAME"
kind = "string"
label = "Username"
default = "guest"

[[targets.main.config_schema]]
name = "PORT"
kind = "number"
default = "8080"

[[targets.main.config_schema]]
name = "MODEL"
kind = "enum"
choices = ["gpt-4", "gpt-5"]
default = "gpt-4"
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse");
    let target = manifest.resolve_default_target().expect("default target");

    let resolved = target.resolved_config_schema();
    assert_eq!(resolved.len(), 4, "config_schema wins over required_env");

    assert_eq!(resolved[0].name, "OPENAI_API_KEY");
    assert!(matches!(resolved[0].kind, ConfigKind::Secret));
    assert_eq!(resolved[0].label.as_deref(), Some("OpenAI API Key"));
    assert_eq!(resolved[0].placeholder.as_deref(), Some("sk-..."));

    assert!(matches!(resolved[1].kind, ConfigKind::String));
    assert_eq!(resolved[1].default.as_deref(), Some("guest"));

    assert!(matches!(resolved[2].kind, ConfigKind::Number));

    match &resolved[3].kind {
        ConfigKind::Enum { choices } => {
            assert_eq!(choices, &vec!["gpt-4".to_string(), "gpt-5".to_string()]);
        }
        other => panic!("expected Enum, got {:?}", other),
    }
}

#[test]
fn config_kind_round_trips_through_toml_and_json() {
    // Round-trip via serde_json (which the E103 details envelope will use on
    // the wire) to guarantee the desktop can reconstruct ConfigField verbatim.
    let field = ConfigField {
        name: "OPENAI_API_KEY".to_string(),
        label: Some("OpenAI API Key".to_string()),
        description: Some("Your key".to_string()),
        kind: ConfigKind::Enum {
            choices: vec!["a".to_string(), "b".to_string()],
        },
        default: Some("a".to_string()),
        placeholder: None,
    };

    let json = serde_json::to_string(&field).expect("serialize");
    let parsed: ConfigField = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, field);

    // Confirm the JSON carries `kind` + `choices` at the top level (flat).
    assert!(json.contains(r#""kind":"enum""#));
    assert!(json.contains(r#""choices":["a","b"]"#));
}

#[test]
fn resolved_config_schema_returns_empty_when_both_fields_absent() {
    let toml = r#"
schema_version = "0.2"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "main"

[targets.main]
runtime = "source"
driver = "node"
entrypoint = "server.js"
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse");
    let target = manifest.resolve_default_target().expect("default target");
    assert!(target.resolved_config_schema().is_empty());
}

#[test]
fn v03_flat_manifest_passes_config_schema_through_to_target() {
    // Feature 2 — the desktop renders its dynamic form from
    // `target.resolved_config_schema()`. v0.3 flat manifests (no
    // `[targets.main]` block) must still surface their `[[config_schema]]`
    // entries on the implicit `main` target after normalization.
    let toml = r#"
schema_version = "0.3"
name = "byok-demo"
version = "0.1.0"
type = "app"
runtime = "web/node"
runtime_version = "20"
run = "npm run dev"
port = 3000

[[config_schema]]
name = "OPENAI_API_KEY"
kind = "secret"
label = "OpenAI API Key"
description = "BYOK"
placeholder = "sk-..."

[[config_schema]]
name = "OPENAI_MODEL"
kind = "enum"
label = "Model"
choices = ["gpt-4o-mini", "gpt-4o"]
default = "gpt-4o-mini"
"#;

    let manifest = CapsuleManifest::from_toml(toml).expect("parse v03 flat with config_schema");
    let target = manifest.resolve_default_target().expect("default target");
    let schema = target.resolved_config_schema();
    assert_eq!(schema.len(), 2, "both rich entries propagate");
    assert_eq!(schema[0].name, "OPENAI_API_KEY");
    assert!(matches!(schema[0].kind, ConfigKind::Secret));
    assert_eq!(schema[0].label.as_deref(), Some("OpenAI API Key"));
    assert_eq!(schema[1].name, "OPENAI_MODEL");
    match &schema[1].kind {
        ConfigKind::Enum { choices } => {
            assert_eq!(
                choices,
                &vec!["gpt-4o-mini".to_string(), "gpt-4o".to_string()]
            );
        }
        other => panic!("expected enum kind, got {other:?}"),
    }
    assert_eq!(schema[1].default.as_deref(), Some("gpt-4o-mini"));
}
