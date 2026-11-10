//! `ato data` — read and write DataResources on a Coordinator.
//!
//! The same JSON contract is served by the hosted API and by the selfhost
//! SQLite Coordinator (`ato-coordinator`). This surface is deliberately thin:
//! it prints what the authority returned and never keeps a local copy, so a
//! revision observed here is the authority's revision.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde_json::{Value, json};

#[derive(Debug, Args)]
pub struct DataArgs {
    #[command(subcommand)]
    pub command: DataCommand,
}

#[derive(Debug, Subcommand)]
pub enum DataCommand {
    /// Report the Coordinator's health.
    Health(CoordinatorArgs),
    /// Inspect or create Resources.
    Resource {
        #[command(subcommand)]
        command: ResourceCommand,
    },
    /// Commit an operation against a Resource.
    Operation {
        #[command(subcommand)]
        command: OperationCommand,
    },
    /// Bind a Resource to a Port and use the binding.
    Binding {
        #[command(subcommand)]
        command: BindingCommand,
    },
    /// Register a Protocol operation with its publisher.
    Protocol {
        #[command(subcommand)]
        command: ProtocolCommand,
    },
    /// Report the Coordinator's quota usage.
    Quota(CoordinatorArgs),
    /// Delete expired operation-result records (retention GC).
    Gc(CoordinatorArgs),
}

#[derive(Debug, Args, Clone)]
pub struct CoordinatorArgs {
    /// Coordinator base URL (hosted API or selfhost Coordinator).
    #[arg(
        long,
        env = "ATO_COORDINATOR_URL",
        default_value = "http://127.0.0.1:8788"
    )]
    pub coordinator: String,
    /// Bearer token for the Coordinator. When omitted, the token stored by
    /// ato login for the same API origin is used, so the CLI and the PWA act
    /// as the same identity.
    #[arg(long, env = "ATO_COORDINATOR_TOKEN")]
    pub token: Option<String>,
    /// Emit the raw JSON response instead of a human-readable summary.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum ResourceCommand {
    /// List the Resources this Coordinator holds for the caller.
    List(CoordinatorArgs),
    /// Read one Resource, including its document.
    Inspect {
        resource_id: String,
        #[command(flatten)]
        coordinator: CoordinatorArgs,
    },
    /// Export one Resource's document with its digest to a JSON file.
    Export {
        resource_id: String,
        #[arg(long)]
        out: PathBuf,
        #[command(flatten)]
        coordinator: CoordinatorArgs,
    },
    /// Import an exported document as a NEW Resource.
    Import {
        #[arg(long)]
        file: PathBuf,
        #[command(flatten)]
        coordinator: CoordinatorArgs,
    },
    /// Create a Resource.
    Create {
        /// Protocol label, e.g. `todo.board@1`.
        #[arg(long, default_value = "ato.data.json@1")]
        protocol: String,
        #[arg(long)]
        display_name: Option<String>,
        /// Document as JSON (defaults to `{}`).
        #[arg(long, default_value = "{}")]
        document: String,
        #[command(flatten)]
        coordinator: CoordinatorArgs,
    },
}

#[derive(Debug, Subcommand)]
pub enum OperationCommand {
    /// Commit a document under compare-and-set, exactly once per operation id.
    Invoke {
        resource_id: String,
        #[arg(long)]
        operation_id: String,
        #[arg(long)]
        expected_revision: i64,
        #[arg(long, default_value_t = 0)]
        expected_generation: i64,
        /// Resulting document as JSON.
        #[arg(long, default_value = "{}")]
        document: String,
        #[command(flatten)]
        coordinator: CoordinatorArgs,
    },
}

#[derive(Debug, Subcommand)]
pub enum BindingCommand {
    /// List bindings (optionally only one app instance's).
    List {
        #[arg(long)]
        instance: Option<String>,
        #[command(flatten)]
        coordinator: CoordinatorArgs,
    },
    /// Bind a Resource to a Port. `--access operations` is operation-only.
    Create {
        resource_id: String,
        #[arg(long)]
        port: String,
        #[arg(long)]
        protocol: String,
        #[arg(long, default_value = "read")]
        access: String,
        /// Operations an operation-only binding may invoke (comma separated).
        #[arg(long, value_delimiter = ',')]
        operations: Vec<String>,
        #[command(flatten)]
        coordinator: CoordinatorArgs,
    },
    /// Read the document through a binding (refused for operation-only).
    Document {
        binding_id: String,
        #[command(flatten)]
        coordinator: CoordinatorArgs,
    },
    /// Invoke an operation through a binding.
    Invoke {
        binding_id: String,
        #[arg(long)]
        operation: String,
        #[arg(long)]
        operation_id: String,
        #[arg(long)]
        expected_revision: i64,
        #[arg(long, default_value_t = 0)]
        expected_generation: i64,
        /// Operation arguments as JSON. When present the Coordinator runs the
        /// registered handler; otherwise the document is committed directly.
        #[arg(long)]
        arguments: Option<String>,
        #[arg(long, default_value = "{}")]
        document: String,
        #[command(flatten)]
        coordinator: CoordinatorArgs,
    },
    /// Revoke a binding.
    Revoke {
        binding_id: String,
        #[command(flatten)]
        coordinator: CoordinatorArgs,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProtocolCommand {
    /// List registered operations (all protocols, or one).
    List {
        #[arg(long)]
        protocol: Option<String>,
        #[command(flatten)]
        coordinator: CoordinatorArgs,
    },
    /// Register one operation of a Protocol (publisher-owned).
    Register {
        protocol: String,
        #[arg(long)]
        operation: String,
        #[arg(long)]
        semantics: String,
        #[arg(long)]
        access: String,
        #[arg(long)]
        implementation_digest: String,
        #[arg(long)]
        publisher: String,
        #[command(flatten)]
        coordinator: CoordinatorArgs,
    },
}

pub fn run(args: DataArgs) -> Result<()> {
    match args.command {
        DataCommand::Health(coordinator) => health(&coordinator),
        DataCommand::Quota(coordinator) => quota(&coordinator),
        DataCommand::Gc(coordinator) => gc(&coordinator),
        DataCommand::Resource { command } => match command {
            ResourceCommand::List(coordinator) => list(&coordinator),
            ResourceCommand::Inspect {
                resource_id,
                coordinator,
            } => inspect(&coordinator, &resource_id),
            ResourceCommand::Export {
                resource_id,
                out,
                coordinator,
            } => resource_export(&coordinator, &resource_id, &out),
            ResourceCommand::Import { file, coordinator } => resource_import(&coordinator, &file),
            ResourceCommand::Create {
                protocol,
                display_name,
                document,
                coordinator,
            } => create(&coordinator, &protocol, display_name, &document),
        },
        DataCommand::Binding { command } => match command {
            BindingCommand::List {
                instance,
                coordinator,
            } => binding_list(&coordinator, instance.as_deref()),
            BindingCommand::Create {
                resource_id,
                port,
                protocol,
                access,
                operations,
                coordinator,
            } => binding_create(
                &coordinator,
                &resource_id,
                &port,
                &protocol,
                &access,
                operations,
            ),
            BindingCommand::Document {
                binding_id,
                coordinator,
            } => binding_document(&coordinator, &binding_id),
            BindingCommand::Invoke {
                binding_id,
                operation,
                operation_id,
                expected_revision,
                expected_generation,
                arguments,
                document,
                coordinator,
            } => binding_invoke(
                &coordinator,
                BindingInvokeInput {
                    binding_id: &binding_id,
                    operation: &operation,
                    operation_id: &operation_id,
                    expected_revision,
                    expected_generation,
                    arguments: arguments.as_deref(),
                    document: &document,
                },
            ),
            BindingCommand::Revoke {
                binding_id,
                coordinator,
            } => binding_revoke(&coordinator, &binding_id),
        },
        DataCommand::Protocol { command } => match command {
            ProtocolCommand::List {
                protocol,
                coordinator,
            } => protocol_list(&coordinator, protocol.as_deref()),
            ProtocolCommand::Register {
                protocol,
                operation,
                semantics,
                access,
                implementation_digest,
                publisher,
                coordinator,
            } => protocol_register(
                &coordinator,
                &protocol,
                &operation,
                &semantics,
                &access,
                &implementation_digest,
                &publisher,
            ),
        },
        DataCommand::Operation { command } => match command {
            OperationCommand::Invoke {
                resource_id,
                operation_id,
                expected_revision,
                expected_generation,
                document,
                coordinator,
            } => invoke(
                &coordinator,
                &resource_id,
                &operation_id,
                expected_revision,
                expected_generation,
                &document,
            ),
        },
    }
}

fn base(coordinator: &CoordinatorArgs) -> String {
    coordinator.coordinator.trim_end_matches('/').to_owned()
}

fn bearer(coordinator: &CoordinatorArgs) -> Result<String> {
    crate::auth_device::resolve_token(coordinator.token.clone(), &coordinator.coordinator)
}

fn request(
    coordinator: &CoordinatorArgs,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("build coordinator client")?;
    let mut request = client
        .request(method, format!("{}{path}", base(coordinator)))
        .header("authorization", format!("Bearer {}", bearer(coordinator)?));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().context("coordinator request failed")?;
    let status = response.status();
    let value: Value = response.json().unwrap_or(Value::Null);
    if !status.is_success() {
        let code = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("coordinator_error");
        bail!("{code} (HTTP {status})");
    }
    Ok(value)
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn health(coordinator: &CoordinatorArgs) -> Result<()> {
    let value = request(coordinator, reqwest::Method::GET, "/health", None)?;
    if coordinator.json {
        return print_json(&value);
    }
    println!(
        "status: {}",
        value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    println!(
        "schema_version: {}",
        value
            .get("schema_version")
            .and_then(Value::as_i64)
            .unwrap_or_default()
    );
    println!(
        "resources: {}",
        value
            .get("resources")
            .and_then(Value::as_i64)
            .unwrap_or_default()
    );
    Ok(())
}

fn list(coordinator: &CoordinatorArgs) -> Result<()> {
    let value = request(
        coordinator,
        reqwest::Method::GET,
        "/v1/data-resources",
        None,
    )?;
    if coordinator.json {
        return print_json(&value);
    }
    let resources = value
        .get("resources")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if resources.is_empty() {
        println!("no resources");
        return Ok(());
    }
    for resource in resources {
        println!(
            "{}  {}  revision {}  {}",
            resource
                .get("resource_id")
                .and_then(Value::as_str)
                .unwrap_or("-"),
            resource
                .get("protocol")
                .and_then(Value::as_str)
                .unwrap_or("-"),
            resource
                .get("revision")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
            resource
                .get("content_digest")
                .and_then(Value::as_str)
                .unwrap_or("-"),
        );
    }
    Ok(())
}

fn inspect(coordinator: &CoordinatorArgs, resource_id: &str) -> Result<()> {
    let value = request(
        coordinator,
        reqwest::Method::GET,
        &format!("/v1/data-resources/{resource_id}"),
        None,
    )?;
    if coordinator.json {
        return print_json(&value);
    }
    println!(
        "resource_id: {}",
        value
            .get("resource_id")
            .and_then(Value::as_str)
            .unwrap_or("-")
    );
    println!(
        "protocol: {}",
        value.get("protocol").and_then(Value::as_str).unwrap_or("-")
    );
    println!(
        "revision: {}",
        value
            .get("revision")
            .and_then(Value::as_i64)
            .unwrap_or_default()
    );
    println!(
        "generation: {}",
        value
            .get("generation")
            .and_then(Value::as_i64)
            .unwrap_or_default()
    );
    println!(
        "content_digest: {}",
        value
            .get("content_digest")
            .and_then(Value::as_str)
            .unwrap_or("-")
    );
    println!(
        "document: {}",
        serde_json::to_string_pretty(value.get("document").unwrap_or(&Value::Null))?
    );
    Ok(())
}

const EXPORT_SCHEMA: &str = "ato.data.export/v1";

fn resource_export(coordinator: &CoordinatorArgs, resource_id: &str, out: &PathBuf) -> Result<()> {
    let snapshot = request(
        coordinator,
        reqwest::Method::GET,
        &format!("/v1/data-resources/{resource_id}/document"),
        None,
    )?;
    let resource_id = snapshot
        .get("resource_id")
        .and_then(Value::as_str)
        .context("Coordinator response has no resource_id")?;
    let protocol = snapshot
        .get("protocol")
        .and_then(Value::as_str)
        .context("Coordinator response has no protocol")?;
    let revision = snapshot
        .get("revision")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let digest = snapshot
        .get("content_digest")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let document = snapshot
        .get("document")
        .cloned()
        .context("Coordinator response has no document")?;
    let export = json!({
        "schema": EXPORT_SCHEMA,
        "resource_id": resource_id,
        "protocol": protocol,
        "revision": revision,
        "content_digest": digest,
        "document": document,
    });
    std::fs::write(out, serde_json::to_vec_pretty(&export)?)
        .with_context(|| format!("write export {}", out.display()))?;
    println!(
        "exported {} revision {} ({}) -> {}",
        resource_id,
        revision,
        digest,
        out.display()
    );
    Ok(())
}

fn resource_import(coordinator: &CoordinatorArgs, file: &PathBuf) -> Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read export {}", file.display()))?;
    let export: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse export {}", file.display()))?;
    if export.get("schema").and_then(Value::as_str) != Some(EXPORT_SCHEMA) {
        bail!("export schema must be {EXPORT_SCHEMA}");
    }
    let protocol = export
        .get("protocol")
        .and_then(Value::as_str)
        .context("export has no protocol")?;
    let document = export
        .get("document")
        .cloned()
        .context("export has no document")?;
    let created = request(
        coordinator,
        reqwest::Method::POST,
        "/v1/data-resources",
        Some(json!({
            "protocol": protocol,
            "display_name": export.get("resource_id").and_then(Value::as_str),
            "document": document,
        })),
    )?;
    println!(
        "imported as {} revision {}",
        created
            .get("resource_id")
            .and_then(Value::as_str)
            .unwrap_or("-"),
        created
            .get("revision")
            .and_then(Value::as_i64)
            .unwrap_or_default()
    );
    Ok(())
}

fn create(
    coordinator: &CoordinatorArgs,
    protocol: &str,
    display_name: Option<String>,
    document: &str,
) -> Result<()> {
    let document: Value = serde_json::from_str(document).context("parse --document as JSON")?;
    let mut body = json!({ "protocol": protocol, "document": document });
    if let Some(display_name) = display_name {
        body["display_name"] = Value::String(display_name);
    }
    let value = request(
        coordinator,
        reqwest::Method::POST,
        "/v1/data-resources",
        Some(body),
    )?;
    if coordinator.json {
        return print_json(&value);
    }
    println!(
        "created {} (revision {})",
        value
            .get("resource_id")
            .and_then(Value::as_str)
            .unwrap_or("-"),
        value
            .get("revision")
            .and_then(Value::as_i64)
            .unwrap_or_default()
    );
    Ok(())
}

fn invoke(
    coordinator: &CoordinatorArgs,
    resource_id: &str,
    operation_id: &str,
    expected_revision: i64,
    expected_generation: i64,
    document: &str,
) -> Result<()> {
    let document: Value = serde_json::from_str(document).context("parse --document as JSON")?;
    let value = request(
        coordinator,
        reqwest::Method::POST,
        &format!("/v1/data-resources/{resource_id}/operations"),
        Some(json!({
            "operation_id": operation_id,
            "expected_revision": expected_revision,
            "expected_generation": expected_generation,
            "document": document,
        })),
    )?;
    if coordinator.json {
        return print_json(&value);
    }
    println!(
        "committed revision {}{}",
        value
            .get("revision")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        if value
            .get("replayed")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            " (replayed)"
        } else {
            ""
        }
    );
    if let Some(receipt) = value.get("receipt") {
        println!("receipt: {}", serde_json::to_string(receipt)?);
    }
    Ok(())
}

fn quota(coordinator: &CoordinatorArgs) -> Result<()> {
    let value = request(
        coordinator,
        reqwest::Method::GET,
        "/v1/data-resources/quota",
        None,
    )?;
    if coordinator.json {
        return print_json(&value);
    }
    println!(
        "used_bytes: {}",
        value
            .get("used_bytes")
            .and_then(Value::as_i64)
            .unwrap_or_default()
    );
    println!(
        "limit_bytes: {}",
        value
            .get("limit_bytes")
            .and_then(Value::as_i64)
            .unwrap_or_default()
    );
    println!(
        "resources: {}",
        value
            .get("resources")
            .and_then(Value::as_i64)
            .unwrap_or_default()
    );
    Ok(())
}

fn gc(coordinator: &CoordinatorArgs) -> Result<()> {
    let value = request(coordinator, reqwest::Method::POST, "/control/gc", None)?;
    if coordinator.json {
        return print_json(&value);
    }
    println!(
        "deleted_results: {}",
        value
            .get("deleted_results")
            .and_then(Value::as_i64)
            .unwrap_or_default()
    );
    Ok(())
}

fn binding_list(coordinator: &CoordinatorArgs, instance: Option<&str>) -> Result<()> {
    let value = request(coordinator, reqwest::Method::GET, "/v1/bindings", None)?;
    if coordinator.json {
        return print_json(&value);
    }
    let bindings = value
        .get("bindings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut shown = 0;
    for binding in bindings {
        let binding_instance = binding.get("instance_id").and_then(Value::as_str);
        if instance.is_some_and(|wanted| binding_instance != Some(wanted)) {
            continue;
        }
        shown += 1;
        let operations = binding
            .get("operations")
            .and_then(Value::as_array)
            .map(|operations| {
                operations
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        println!(
            "{}  {}  {}  {}  {}  {}",
            binding
                .get("binding_id")
                .and_then(Value::as_str)
                .unwrap_or("-"),
            binding
                .get("port_name")
                .and_then(Value::as_str)
                .unwrap_or("-"),
            binding.get("access").and_then(Value::as_str).unwrap_or("-"),
            if operations.is_empty() {
                "-".to_owned()
            } else {
                operations
            },
            binding_instance.unwrap_or("owner"),
            binding.get("status").and_then(Value::as_str).unwrap_or("-"),
        );
    }
    if shown == 0 {
        println!("no bindings");
    }
    Ok(())
}

fn binding_create(
    coordinator: &CoordinatorArgs,
    resource_id: &str,
    port: &str,
    protocol: &str,
    access: &str,
    operations: Vec<String>,
) -> Result<()> {
    let value = request(
        coordinator,
        reqwest::Method::POST,
        &format!("/v1/data-resources/{resource_id}/bindings"),
        Some(json!({
            "port": port,
            "protocol": protocol,
            "access": access,
            "operations": operations,
        })),
    )?;
    if coordinator.json {
        return print_json(&value);
    }
    println!(
        "bound {} (access {}, operations: {})",
        value
            .get("binding_id")
            .and_then(Value::as_str)
            .unwrap_or("-"),
        value.get("access").and_then(Value::as_str).unwrap_or("-"),
        value
            .get("operations")
            .and_then(Value::as_array)
            .map(|operations| operations
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", "))
            .unwrap_or_default()
    );
    Ok(())
}

fn binding_document(coordinator: &CoordinatorArgs, binding_id: &str) -> Result<()> {
    let value = request(
        coordinator,
        reqwest::Method::GET,
        &format!("/v1/bindings/{binding_id}/document"),
        None,
    )?;
    if coordinator.json {
        return print_json(&value);
    }
    println!(
        "revision: {}",
        value
            .get("revision")
            .and_then(Value::as_i64)
            .unwrap_or_default()
    );
    println!(
        "document: {}",
        serde_json::to_string_pretty(value.get("document").unwrap_or(&Value::Null))?
    );
    Ok(())
}

struct BindingInvokeInput<'a> {
    binding_id: &'a str,
    operation: &'a str,
    operation_id: &'a str,
    expected_revision: i64,
    expected_generation: i64,
    arguments: Option<&'a str>,
    document: &'a str,
}

fn binding_invoke(coordinator: &CoordinatorArgs, input: BindingInvokeInput<'_>) -> Result<()> {
    let mut body = json!({
        "operation": input.operation,
        "operation_id": input.operation_id,
        "expected_revision": input.expected_revision,
        "expected_generation": input.expected_generation,
    });
    match input.arguments {
        Some(arguments) => {
            body["arguments"] =
                serde_json::from_str(arguments).context("parse --arguments as JSON")?;
        }
        None => {
            body["document"] =
                serde_json::from_str(input.document).context("parse --document as JSON")?;
        }
    }
    let value = request(
        coordinator,
        reqwest::Method::POST,
        &format!("/v1/bindings/{}/operations", input.binding_id),
        Some(body),
    )?;
    if coordinator.json {
        return print_json(&value);
    }
    println!(
        "committed revision {}{}",
        value
            .get("revision")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        if value
            .get("replayed")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            " (replayed)"
        } else {
            ""
        }
    );
    Ok(())
}

fn binding_revoke(coordinator: &CoordinatorArgs, binding_id: &str) -> Result<()> {
    let value = request(
        coordinator,
        reqwest::Method::DELETE,
        &format!("/v1/bindings/{binding_id}"),
        None,
    )?;
    if coordinator.json {
        return print_json(&value);
    }
    println!(
        "binding {}: {}",
        binding_id,
        value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    Ok(())
}

fn protocol_list(coordinator: &CoordinatorArgs, protocol: Option<&str>) -> Result<()> {
    let path = match protocol {
        Some(protocol) => format!("/v1/protocols/{}/operations", protocol.replace('/', "%2F")),
        None => "/v1/operations".to_owned(),
    };
    let value = request(coordinator, reqwest::Method::GET, &path, None)?;
    if coordinator.json {
        return print_json(&value);
    }
    let operations = value
        .get("operations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for operation in operations {
        let digest = operation
            .get("implementation_digest")
            .and_then(Value::as_str)
            .unwrap_or("-");
        println!(
            "{}  {}  {}  {}  {}  {}",
            operation
                .get("protocol")
                .and_then(Value::as_str)
                .unwrap_or("-"),
            operation
                .get("operation")
                .and_then(Value::as_str)
                .unwrap_or("-"),
            operation
                .get("semantics")
                .and_then(Value::as_str)
                .unwrap_or("-"),
            operation
                .get("access")
                .and_then(Value::as_str)
                .unwrap_or("-"),
            if digest.len() > 19 {
                &digest[..19]
            } else {
                digest
            },
            if operation
                .get("expose_mcp")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                "mcp"
            } else {
                "-"
            },
        );
    }
    Ok(())
}

fn protocol_register(
    coordinator: &CoordinatorArgs,
    protocol: &str,
    operation: &str,
    semantics: &str,
    access: &str,
    implementation_digest: &str,
    publisher: &str,
) -> Result<()> {
    let value = request(
        coordinator,
        reqwest::Method::POST,
        &format!("/v1/protocols/{protocol}/operations"),
        Some(json!({
            "operation": operation,
            "semantics": semantics,
            "access": access,
            "implementation_digest": implementation_digest,
            "publisher": publisher,
        })),
    )?;
    if coordinator.json {
        return print_json(&value);
    }
    println!("registered {} on {}", operation, protocol);
    Ok(())
}
