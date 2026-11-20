//! Reproduce the `load_config` wiring gate: capsule.toml declarations that the
//! sealing path accepts but `capsule.compose@1` validation (lib/compose) rejects.
//!
//! Build: rustc --edition 2024 <file> \
//!   --extern ato_local_execution=target/debug/deps/libato_local_execution-<hash>.rlib \
//!   -L dependency=target/debug/deps -o <out>
use std::fs;
use std::path::{Path, PathBuf};

const BASE: &str = "schema = 1\n\n[[process]]\nid = \"a\"\ncommand = [\"a\"]\n\n[[process]]\nid = \"b\"\ncommand = [\"b\"]\n\n[[process]]\nid = \"c\"\ncommand = [\"c\"]\n\n[[adapter]]\ntarget = \"a\"\nuse = \"ato.process@1\"\n\n[[port]]\nid = \"a.out\"\nnode = \"a\"\nprotocol = \"fixture.text@1\"\nrole = \"client\"\ninternal = true\nenvironment = \"PEER\"\n\n[[port]]\nid = \"b.in\"\nnode = \"b\"\nprotocol = \"fixture.text@1\"\nrole = \"server\"\ninternal = true\naddress = \"127.0.0.1:7001\"\n\n[[port]]\nid = \"c.in\"\nnode = \"c\"\nprotocol = \"fixture.text@1\"\nrole = \"server\"\ninternal = true\naddress = \"127.0.0.1:7002\"\n";

fn project(name: &str, body: &str) -> PathBuf {
    let dir = Path::new(".")
        .join(".tmp")
        .join("notes")
        .join("semantic-core")
        .join("gate")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("capsule.toml"), body).unwrap();
    dir
}

fn main() {
    let out = std::env::current_dir().unwrap();
    assert!(out.ends_with("lean-bugcheck"), "run from the worktree root");

    // W3: one internal client Port bound to two internal servers.
    let multi = project(
        "multi",
        &format!(
            "{BASE}\n[[connection]]\nfrom = \"a.out\"\nto = \"b.in\"\n\n[[connection]]\nfrom = \"a.out\"\nto = \"c.in\"\n"
        ),
    );
    let config = ato_local_execution::authoring::load_config(&multi)
        .expect("load_config accepts a client Port bound to two peers");
    assert_eq!(config.connection.len(), 2);
    println!("multi-bound client Port: load_config accepts 2 connections from a.out");

    // ... and the runtime projection silently keeps the last declared peer.
    let peer = |config: &ato_local_execution::authoring::AuthoringConfig| -> String {
        let instances = ato_local_execution::authoring::adapter_instances(
            config,
            &std::collections::BTreeMap::new(),
            false,
            false,
        )
        .unwrap();
        let instance = instances
            .iter()
            .find(|instance| instance.adapter_id == "ato.process@1")
            .expect("a process adapter instance");
        instance.config["environment"]["PEER"]
            .as_str()
            .expect("PEER projection")
            .to_owned()
    };
    assert_eq!(peer(&config), "127.0.0.1:7002");
    let swapped = project(
        "multi-swapped",
        &format!(
            "{BASE}\n[[connection]]\nfrom = \"a.out\"\nto = \"c.in\"\n\n[[connection]]\nfrom = \"a.out\"\nto = \"b.in\"\n"
        ),
    );
    let swapped_config = ato_local_execution::authoring::load_config(&swapped).unwrap();
    assert_eq!(peer(&swapped_config), "127.0.0.1:7001");
    println!("multi-bound client Port: PEER projection is the last declared peer (7002 vs 7001)");

    // W1: the same connection declared twice.
    let dup = project(
        "dup",
        &format!(
            "{BASE}\n[[connection]]\nfrom = \"a.out\"\nto = \"b.in\"\n\n[[connection]]\nfrom = \"a.out\"\nto = \"b.in\"\n"
        ),
    );
    let config = ato_local_execution::authoring::load_config(&dup)
        .expect("load_config accepts a duplicated connection");
    assert_eq!(config.connection.len(), 2);
    println!("duplicate connection: load_config accepts the same edge twice");

    // W2: two internal Ports of one process, wired to each other.
    let same_node = "schema = 1\n\n[[process]]\nid = \"a\"\ncommand = [\"a\"]\n\n[[adapter]]\ntarget = \"a\"\nuse = \"ato.process@1\"\n\n[[port]]\nid = \"a.left\"\nnode = \"a\"\nprotocol = \"fixture.text@1\"\nrole = \"client\"\ninternal = true\nenvironment = \"PEER\"\n\n[[port]]\nid = \"a.right\"\nnode = \"a\"\nprotocol = \"fixture.text@1\"\nrole = \"server\"\ninternal = true\naddress = \"127.0.0.1:7003\"\n\n[[connection]]\nfrom = \"a.left\"\nto = \"a.right\"\n";
    let node = project("same-node", same_node);
    let config = ato_local_execution::authoring::load_config(&node)
        .expect("load_config accepts a connection between two Ports of one process");
    assert_eq!(config.connection.len(), 1);
    println!("same-node connection: load_config accepts a.left -> a.right");

    // Positive controls: the checks load_config does perform still fire.
    let self_edge = project(
        "self",
        &format!(
            "{BASE}\n[[connection]]\nfrom = \"a.out\"\nto = \"a.out\"\n"
        ),
    );
    let error = ato_local_execution::authoring::load_config(&self_edge)
        .expect_err("a self connection is rejected");
    println!("control: self connection rejected: {error}");

    let unwired = project(
        "unwired",
        "{BASE}",
    );
    let error = ato_local_execution::authoring::load_config(&unwired)
        .expect_err("an unwired internal client Port is rejected");
    println!("control: unwired client rejected: {error}");

    println!("load_config is weaker than capsule.compose@1 validation");
}