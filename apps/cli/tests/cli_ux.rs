use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn ato() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ato"))
}

fn assert_success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
}

#[test]
fn help_groups_expose_public_tasks_and_the_init_example_works() {
    let help = assert_success(ato().arg("--help").output().unwrap());
    let (_, commands) = help.split_once("Main commands:\n").unwrap();
    let names: Vec<_> = commands
        .lines()
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert_eq!(names, ["run", "app", "form", "hub", "login"]);
    assert!(help.contains("--help-all"));
    assert!(!help.contains("form-session"));

    let help = assert_success(ato().arg("--help-all").output().unwrap());
    let use_apps = help.find("Use apps:").unwrap();
    let authoring = help.find("Author a recorded project:").unwrap();
    let tools = help.find("Formation and runtime tools:").unwrap();
    assert!(help[use_apps..authoring].contains("  run "));
    assert!(help[tools..].contains("  form-session "));
    for namespace in ["formation", "project", "runtime"] {
        assert!(help.contains(&format!("  {namespace} ")), "{help}");
    }
    assert!(!help.contains("__worker"));

    let root = tempfile::tempdir().unwrap();
    let help = assert_success(ato().args(["init", "--help"]).output().unwrap());
    let (_, example) = help.split_once("Example project/capsule.toml").unwrap();
    let (manifest, _) = example.split_once("Initialize:").unwrap();
    let manifest = manifest
        .lines()
        .skip(1)
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n");
    for (directory, command) in [("legacy", &["init"][..]), ("project", &["project", "init"])] {
        let project = root.path().join(directory);
        fs::create_dir(&project).unwrap();
        fs::write(project.join("capsule.toml"), &manifest).unwrap();
        assert_success(
            ato()
                .args(command)
                .arg("--initial-only")
                .arg(&project)
                .output()
                .unwrap(),
        );
        assert!(project.join(".capsule/refs/heads/main").is_file());
    }
}

#[test]
fn declared_connections_can_be_found_for_a_bundle_and_an_installed_app() {
    let root = tempfile::tempdir().unwrap();
    let capsule = root.path().join("mail.capsule");
    let (bytes, _) =
        ato_portable_application::build_authored_bundle_v2(&sample("ato-mail")).unwrap();
    fs::write(&capsule, bytes).unwrap();
    let declarations = assert_success(
        ato()
            .env("ATO_HOME", root.path())
            .args(["run", "--list-bindings", "--json"])
            .arg(&capsule)
            .output()
            .unwrap(),
    );
    let bindings: Vec<Value> = serde_json::from_str(&declarations).unwrap();
    assert!(bindings.iter().any(|binding| {
        binding == &serde_json::json!({"id":"relay_auth","protocol":"ato.secret@1","required":true})
    }));
    assert!(!root.path().join("cache").exists());

    let metadata = assert_success(
        ato()
            .env("ATO_HOME", root.path())
            .args(["app", "import"])
            .arg(capsule)
            .output()
            .unwrap(),
    );
    let instance: Value = serde_json::from_str(&metadata).unwrap();
    let id = instance["instance_id"].as_str().unwrap();
    let installed = assert_success(
        ato()
            .env("ATO_HOME", root.path())
            .args(["app", "start", id, "--list-bindings"])
            .output()
            .unwrap(),
    );
    assert_eq!(installed, declarations);
    assert!(
        !root
            .path()
            .join("instances")
            .join(id)
            .join("active-run.json")
            .exists()
    );
}

/// One single-container OCI route with two required Bindings, plus a fake
/// Docker CLI that answers probes only. Any command that would change
/// something is recorded and refused, so a test can assert nothing started.
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct PortableOciFixture {
    root: tempfile::TempDir,
    bundle: PathBuf,
    home: PathBuf,
    mutations: PathBuf,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl PortableOciFixture {
    fn new(rootless: bool, endpoint: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let security_options = if rootless {
            r#"["name=rootless"]"#
        } else {
            "[]"
        };

        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        fs::create_dir(&project).unwrap();
        let manifest = fs::read_to_string(sample("portable-oci-startup-json").join("capsule.toml"))
            .unwrap()
            .replace(
                "[contract]",
                r#"[[binding]]
id = "admin_password"
protocol = "ato.secret@1"
required = true

[[binding]]
id = "site_name"
protocol = "ato.config@1"
required = true

[contract]"#,
            );
        fs::write(project.join("capsule.toml"), &manifest).unwrap();
        fs::write(project.join("fixture.txt"), "a non-empty workspace").unwrap();
        let bundle = root.path().join("test app.capsule");
        let (bytes, _) = ato_portable_application::build_authored_bundle_v2(&project).unwrap();
        fs::write(&bundle, bytes).unwrap();

        let docker = root.path().join("docker");
        fs::write(
            &docker,
            format!(
                r#"#!/bin/sh
case "$1 $2" in
  "context show") printf '%s\n' default ;;
  "context inspect") printf '%s\n' '{endpoint}' ;;
  "info --format")
    printf '%s\n' '{{"OSType":"linux","Architecture":"x86_64","OperatingSystem":"Test Linux","SecurityOptions":{security_options}}}'
    ;;
  *) printf '%s\n' "$1" >> "$ATO_TEST_DOCKER_MUTATIONS"; exit 1 ;;
esac
"#
            ),
        )
        .unwrap();
        fs::set_permissions(&docker, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            home: root.path().join("home"),
            mutations: root.path().join("docker-mutations"),
            bundle,
            root,
        }
    }

    fn run(&self) -> Command {
        let mut command = ato();
        command
            .env("PATH", self.root.path())
            .env("ATO_HOME", &self.home)
            .env("ATO_TEST_DOCKER_MUTATIONS", &self.mutations)
            .env_remove("DOCKER_HOST")
            .env_remove("DOCKER_CONTEXT")
            .arg("run")
            .arg(&self.bundle)
            .arg("--no-open");
        command
    }

    fn routes(&self) -> Value {
        serde_json::from_str(&assert_success(
            self.run()
                .args(["--list-derivations", "--json"])
                .output()
                .unwrap(),
        ))
        .unwrap()
    }
}

/// A remote daemon cannot receive this Run's workspace, so the route is
/// refused before the CLI asks for any of the Bindings it declares.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn run_refuses_an_incompatible_container_before_requesting_bindings() {
    let container = PortableOciFixture::new(false, "tcp://10.0.0.5:2375");
    let output = container.run().output().unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("from another machine"), "{error}");
    assert!(!error.contains("admin_password"), "{error}");
    assert!(!container.mutations.exists());
    assert_eq!(
        fs::read_dir(container.home.join("cache")).unwrap().count(),
        0
    );
    assert_eq!(container.routes()["derivations"][0]["runnable"], false);
}

/// A rootless daemon used to refuse a single container outright. The Run's own
/// relay reaches that container's Port, so the route is admitted and the CLI
/// moves on to the Bindings it declares — still without starting a container.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn run_admits_a_single_container_on_a_rootless_daemon() {
    let container = PortableOciFixture::new(true, "unix:///var/run/docker.sock");
    let output = container.run().output().unwrap();
    assert!(
        !output.status.success(),
        "the required Bindings are missing"
    );
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("Required Bindings are missing"), "{error}");
    assert!(!error.contains("rootless mode"), "{error}");
    assert!(
        !container.mutations.exists(),
        "no container command may run"
    );
    assert_eq!(container.routes()["derivations"][0]["runnable"], true);
}

#[test]
fn hub_connection_failure_has_a_short_default_and_verbose_details() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);

    for verbose in [false, true] {
        let mut command = ato();
        command.env("ATO_HOME", root.path()).args([
            "run",
            "capsules/example/missing",
            "--api-url",
            &api,
        ]);
        if verbose {
            command.arg("--verbose");
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(&api), "{error}");
        if verbose {
            assert!(error.contains("/v1/hub/capsules"), "{error}");
        } else {
            assert!(
                error.contains("Could not fetch") && error.contains("--verbose"),
                "{error}"
            );
            assert!(!error.contains("/v1/hub/capsules"), "{error}");
        }
    }
}

#[test]
fn form_inspection_preserves_the_scriptable_json_document() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("package.json"),
        r#"{"scripts":{"build":"vite build"}}"#,
    )
    .unwrap();
    for (entry, json) in [
        (&["form-inspect"][..], false),
        (&["form-inspect"][..], true),
        (&["formation", "inspect"][..], true),
    ] {
        let mut command = ato();
        command.args(entry).arg(root.path());
        if json {
            command.arg("--json");
        }
        let stdout = assert_success(command.output().unwrap());
        let value: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(value["repository"]["runtime_verified"], false);
        assert_eq!(value["declared_execution"], Value::Null);
    }
}

#[test]
fn usage_errors_offer_relevant_public_commands() {
    for (args, expected) in [
        (&["status"][..], "ato app list"),
        (&["doctor"][..], "ato data health"),
        (&["hub", "search"][..], "https://ato.run/capsules"),
        (&["ap"][..], "subcommand exists: 'app'"),
    ] {
        let output = ato().args(args).output().unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2));
        assert!(
            error.contains(expected) && !error.contains("__static-bundle"),
            "{error}"
        );
    }
}

#[test]
fn undeclared_route_error_lists_actual_references_and_the_next_command() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("test app.capsule");
    fs::copy(sample("interop-multi-derivation.capsule"), &file).unwrap();
    let output = ato()
        .env("ATO_HOME", root.path())
        .args(["run", "--derivation", "help"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    let routes = assert_success(
        ato()
            .env("ATO_HOME", root.path())
            .args(["run", "--list-derivations", "--json"])
            .arg(&file)
            .output()
            .unwrap(),
    );
    let routes: Value = serde_json::from_str(&routes).unwrap();
    for route in routes["derivations"].as_array().unwrap() {
        assert!(
            error.contains(route["derivation_ref"].as_str().unwrap()),
            "{error}"
        );
    }
    assert!(
        error.contains(&format!("ato run '{}' --list-derivations", file.display())),
        "{error}"
    );
}

#[test]
fn documented_specialist_entry_points_keep_their_existing_shape() {
    for (args, required) in [
        (&["form-input", "--help"][..], "search_id"),
        (&["form-session", "--help"][..], "--session-bridge"),
        (
            &["form-verify", "--help"][..],
            "--authorize-functional-verification",
        ),
        (&["runtime-network", "serve", "--help"][..], "--token-file"),
        (&["model-set", "import", "--help"][..], "--manifest"),
        (
            &["formation", "verify", "--help"][..],
            "--authorize-functional-verification",
        ),
        (
            &["runtime", "network", "serve", "--help"][..],
            "--token-file",
        ),
        (
            &["runtime", "model-set", "import", "--help"][..],
            "--manifest",
        ),
    ] {
        let help = assert_success(ato().args(args).output().unwrap());
        assert!(help.contains(required), "{help}");
    }
}
