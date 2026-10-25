use std::collections::BTreeMap;

use ato_computation::{
    Boundary, ComputationObject, PortDef, PortId, ProtocolId, RoleId, SemanticsId, computation_ref,
    encode_computation_object,
};
use ato_objects::{MemoryObjectStore, ObjectStore};

use super::*;

fn fixture() -> Result<(MemoryObjectStore, ComputationRef)> {
    let objects = MemoryObjectStore::default();
    let server = objects.put(b"print('2048')")?;
    let snapshot = objects.put(&serde_jcs::to_vec(&WorkspaceSnapshot {
        files: BTreeMap::from([("server.py".to_owned(), server.to_string())]),
    })?)?;
    let state = objects.put(&serde_jcs::to_vec(&serde_json::json!({
        "version": 1,
        "config": {
            "schema": 1,
            "process": [{
                "id":"app",
                "command":["python3","server.py"],
                "cwd":".",
                "capture":"unsupported"
            }],
            "adapter": [{
                "use":"ato.http@1",
                "config":null,
                "target":"app",
                "port":"web",
                "listen":"127.0.0.1:38865",
                "upstream":"127.0.0.1:38866",
                "ready_path":"/"
            }],
            "port": [{
                "id":"web",
                "node":"app",
                "protocol":"ato.http@1",
                "role":"server"
            }],
            "connection": [],
            "binding": [],
            "workspace": {},
            "encap": {}
        },
        "workspace_snapshot": snapshot.to_string()
    }))?)?;
    let object = ComputationObject {
        semantics: SemanticsId::parse(AUTHORING_SEMANTICS_ID)?,
        boundary: Boundary::from([(
            PortId::parse("web")?,
            PortDef {
                protocol: ProtocolId::parse("ato.http@1")?,
                role: RoleId::parse("server")?,
            },
        )]),
        residual: state,
    };
    let target = computation_ref(&object)?;
    objects.insert(target.content_ref(), &encode_computation_object(&object)?)?;
    Ok((objects, target))
}

fn profile(temp: &Path) -> GuestPhysicalBuildProfile {
    let agent = temp.join("ato-guest-agent");
    let kernel = temp.join("vmlinux");
    let mke2fs = temp.join("mke2fs");
    fs::write(&agent, b"agent").unwrap();
    fs::write(&kernel, b"kernel").unwrap();
    fs::write(&mke2fs, b"tool").unwrap();
    GuestPhysicalBuildProfile {
        stdio_mode: StdioMode::Log,
        pixel_surface: None,
        base_image: "docker.io/library/python@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        guest_agent: agent,
        kernel,
        image_size_mib: 512,
        network: GuestNetwork {
            guest_ip: "172.30.0.2".to_owned(),
            host_ip: "172.30.0.1".to_owned(),
            netmask: "255.255.255.0".to_owned(),
        },
        container_tool: "docker".to_owned(),
        mke2fs,
    }
}

#[test]
fn derives_process_workspace_and_http_relay_from_computation() -> Result<()> {
    let (objects, target) = fixture()?;
    let temp = tempfile::tempdir()?;
    let plan = derive_guest_build_plan(&target, &objects, &profile(temp.path()))?;
    assert_eq!(plan.target_computation_ref, target.to_string());
    assert_eq!(plan.workspace.file_count, 1);
    assert_eq!(plan.processes[0].command, ["python3", "server.py"]);
    assert_eq!(plan.processes[0].cwd, "/workspace");
    assert_eq!(plan.http_relays[0].guest_port, 38865);
    assert_eq!(plan.http_relays[0].target.to_string(), "127.0.0.1:38866");
    Ok(())
}

#[test]
fn rejects_floating_base_image_and_host_workspace_paths() -> Result<()> {
    let (objects, target) = fixture()?;
    let temp = tempfile::tempdir()?;
    let mut physical = profile(temp.path());
    physical.base_image = "python:3.11-slim".to_owned();
    assert!(derive_guest_build_plan(&target, &objects, &physical).is_err());

    assert!(validate_relative_path(Path::new("../secret")).is_err());
    assert!(validate_relative_path(Path::new("/Users/me/.env")).is_err());
    Ok(())
}

#[test]
fn init_uses_semantic_relay_and_zero_binding_supervisor() -> Result<()> {
    let (objects, target) = fixture()?;
    let temp = tempfile::tempdir()?;
    let plan = derive_guest_build_plan(&target, &objects, &profile(temp.path()))?;
    let init = init_script(&plan);
    assert!(init.contains("--listen-guest-port 38865 --target 127.0.0.1:38866"));
    assert!(init.contains("ATO_GUEST_AGENT_MODE=vsock"));
    let supervisor = supervisor_config(&plan)?;
    assert!(supervisor.bindings_env.is_empty());
    assert!(supervisor.services.is_empty());
    Ok(())
}

#[test]
fn native_materialization_keeps_computation_and_workspace_identity() -> Result<()> {
    let (objects, target) = fixture()?;
    let temp = tempfile::tempdir()?;
    let baseline = derive_guest_build_plan(&target, &objects, &profile(temp.path()))?;
    assert!(
        serde_json::to_value(&baseline)?
            .get("pixel_surface")
            .is_none()
    );
    assert!(serde_json::to_value(&baseline)?.get("stdio_mode").is_none());
    let mut native = profile(temp.path());
    native.pixel_surface = Some(PixelProducerConfig {
        width: 1280,
        height: 720,
        rfb_port: 5900,
        display: 99,
    });
    let gui = derive_guest_build_plan(&target, &objects, &native)?;
    assert_eq!(gui.target_computation_ref, baseline.target_computation_ref);
    assert_eq!(gui.workspace, baseline.workspace);
    assert_eq!(gui.processes, baseline.processes);
    assert_eq!(supervisor_config(&gui)?.pixel_surface, native.pixel_surface);
    assert!(
        init_script(&gui).contains("tcp-relay --listen-guest-port 5900 --target 127.0.0.1:5900")
    );
    assert!(dockerfile(&gui.base_image, true).contains("command -v Xtigervnc"));
    native.pixel_surface = None;
    native.stdio_mode = StdioMode::Pty;
    let tui = derive_guest_build_plan(&target, &objects, &native)?;
    assert_eq!(supervisor_config(&tui)?.stdio_mode, StdioMode::Pty);
    assert_eq!(tui.target_computation_ref, baseline.target_computation_ref);
    Ok(())
}

#[test]
fn native_profile_refuses_conflicting_surfaces_and_relay_ports() -> Result<()> {
    let (objects, target) = fixture()?;
    let temp = tempfile::tempdir()?;
    let mut native = profile(temp.path());
    native.pixel_surface = Some(PixelProducerConfig {
        width: 1280,
        height: 720,
        rfb_port: 5900,
        display: 99,
    });
    native.stdio_mode = StdioMode::Pty;
    assert!(derive_guest_build_plan(&target, &objects, &native).is_err());
    native.stdio_mode = StdioMode::Log;
    let baseline = derive_guest_build_plan(&target, &objects, &native)?;
    native.pixel_surface.as_mut().unwrap().rfb_port = baseline.http_relays[0].guest_port;
    assert!(derive_guest_build_plan(&target, &objects, &native).is_err());
    native.pixel_surface.as_mut().unwrap().width = 0;
    assert!(derive_guest_build_plan(&target, &objects, &native).is_err());
    Ok(())
}
