use std::fs;
use std::process::Command;

use anyhow::{Context, Result};
use serde_json::Value;

#[test]
fn project_infer_manifest_emits_node_manifest_json() -> Result<()> {
    let dir = tempfile::tempdir().context("tempdir")?;
    fs::write(
        dir.path().join("package.json"),
        r#"{
            "name": "dock-node-demo",
            "version": "1.2.3",
            "scripts": {
                "start": "node server.js"
            }
        }"#,
    )
    .context("package.json")?;
    fs::write(dir.path().join("server.js"), "console.log('ok');\n").context("server.js")?;

    let output = Command::new(assert_cmd::cargo::cargo_bin("ato"))
        .arg("project")
        .arg("infer-manifest")
        .arg(dir.path())
        .arg("--json")
        .output()
        .context("run ato project infer-manifest")?;

    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let payload: Value = serde_json::from_slice(&output.stdout).context("parse json")?;
    let manifest = payload
        .get("manifest_toml")
        .and_then(Value::as_str)
        .context("manifest_toml")?;

    assert_eq!(payload["inference_mode"], "static_inference");
    assert!(manifest.contains("name = \"dock-node-demo\""));
    assert!(manifest.contains("version = \"1.2.3\""));
    assert!(manifest.contains("driver = \"node\""));
    assert!(
        manifest.contains("server.js")
            || manifest.contains("npm start")
            || manifest.contains("npm run start"),
        "{manifest}"
    );

    Ok(())
}

#[test]
fn project_infer_manifest_emits_python_manifest_json() -> Result<()> {
    let dir = tempfile::tempdir().context("tempdir")?;
    fs::write(dir.path().join("main.py"), "print('ok')\n").context("main.py")?;

    let output = Command::new(assert_cmd::cargo::cargo_bin("ato"))
        .arg("project")
        .arg("infer-manifest")
        .arg(dir.path())
        .arg("--json")
        .output()
        .context("run ato project infer-manifest")?;

    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let payload: Value = serde_json::from_slice(&output.stdout).context("parse json")?;
    let manifest = payload
        .get("manifest_toml")
        .and_then(Value::as_str)
        .context("manifest_toml")?;

    assert!(manifest.contains("runtime = \"source\""));
    assert!(manifest.contains("driver = \"python\""));
    assert!(manifest.contains("main.py"));

    Ok(())
}
