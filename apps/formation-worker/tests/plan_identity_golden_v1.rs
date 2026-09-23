//! Routes without `exec` keep their identities bit for bit.
//!
//! The values below were measured with the code from before routes could
//! hold `exec` steps (feb839e6: main c0de5efb plus #1387, which does not
//! touch planning). Adding step networks, step cwd/env on build steps and
//! the projection of preparation steps must not move any of them: a changed
//! `DerivationRef` would re-identify every recorded route, and a changed plan
//! digest every formation key.

use std::collections::BTreeMap;

use ato_formation::capsule_toml::parse_capsule_toml;
use ato_formation::detect::detect;
use ato_formation::source::{RESOLVER_CONTRACT_V1, SourceClosureRef};
use ato_formation_worker::job::plan_candidate;

fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, contents) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }
    dir
}

/// `(derivation_ref, intent_digest, plan_digest)` of an authored route.
fn identities(files: &[(&str, &str)]) -> (String, String, String) {
    let dir = tree(files);
    let text = files
        .iter()
        .find(|(name, _)| *name == "capsule.toml")
        .expect("an authored route")
        .1;
    let draft = parse_capsule_toml(text).expect("parses");
    let closure = SourceClosureRef::derive(
        "sha256:1111111111111111111111111111111111111111111111111111111111111111",
        "",
        RESOLVER_CONTRACT_V1,
    )
    .expect("closure");
    let evidence = detect(dir.path()).expect("detects");
    let planned = plan_candidate(
        &draft,
        &closure,
        &evidence,
        BTreeMap::new(),
        "/app",
        "x86_64-linux-gnu",
    )
    .expect("plans");
    (
        planned.derivation_ref,
        planned.intent_digest,
        planned.plan_digest,
    )
}

const PROCESS: &str = r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[runtime]]
name = "python"
version = "3.12.7"

[[derive.step]]
id = "app"
use = "ato.process@1"
op = "serve"
argv = ["/opt/ato/toolchains/python/3.12.7/bin/python3", "-B", "app.py", "a b"]
cwd = "server"

[derive.step.env]
APP_MODE = "production"

[[port]]
id = "app.http"
use = "ato.http@1"
from = "app"
guest_port = 8000

[[contract.require]]
id = "app-responds"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "/health"

[contract.require.expect]
status = 200
"#;

const STATIC: &str = r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[derive.step]]
id = "site"
use = "ato.browser@1"
op = "serve"
source = "workspace"
entry = "index.html"
spa_fallback = false

[[port]]
id = "app.http"
use = "ato.http@1"
from = "site"

[[contract.require]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "/"

[contract.require.expect]
status = 200
"#;

#[test]
fn a_process_route_with_pip_dependencies_keeps_its_identities() {
    let got = identities(&[
        ("capsule.toml", PROCESS),
        ("requirements.txt", "flask==3.0.0\n"),
        ("app.py", "print('x')\n"),
    ]);
    assert_eq!(got, (PIP.0.to_owned(), PIP.1.to_owned(), PIP.2.to_owned()));
}

#[test]
fn a_process_route_with_a_uv_lock_keeps_its_identities() {
    let got = identities(&[
        ("capsule.toml", PROCESS),
        (
            "pyproject.toml",
            "[project]\nname = \"a\"\nrequires-python = \">=3.12\"\n",
        ),
        ("uv.lock", "version = 1\n"),
        ("app.py", "print('x')\n"),
    ]);
    assert_eq!(got, (UV.0.to_owned(), UV.1.to_owned(), UV.2.to_owned()));
}

#[test]
fn a_static_route_keeps_its_identities() {
    let got = identities(&[("capsule.toml", STATIC), ("index.html", "<!doctype html>")]);
    assert_eq!(
        got,
        (
            STATIC_IDS.0.to_owned(),
            STATIC_IDS.1.to_owned(),
            STATIC_IDS.2.to_owned()
        )
    );
}

const PIP: (&str, &str, &str) = (
    "sha256:ed505fd1e9450b0acdaf20e9946add35786c33f8ac84dc23ea6e6fec05f56789",
    "sha256:4caaa19c7af0a803d10e3f734e8fe8a122c83bad5dfde3e71f3be08fc1767397",
    "sha256:0037b1cef2a113c5d53c0a10f5d034c5aea257dd2a0c5812822082952abfd73c",
);
const UV: (&str, &str, &str) = (
    "sha256:ed505fd1e9450b0acdaf20e9946add35786c33f8ac84dc23ea6e6fec05f56789",
    "sha256:3e82b2bda709dfa6cc041c7f6dce01a2f3747886764575c21502698ef3cd8afc",
    "sha256:e270221be3f1fe2e2bd7b0b793be42b7eb63e99f0a4184b78173f1face19d5ca",
);
const STATIC_IDS: (&str, &str, &str) = (
    "sha256:5204635270982e40d53fc851cdf017b2254b334535e09b37e308cae66237c4cf",
    "sha256:cec9b08b5aac8693e7fd69a567a790f11a389624a21c404db1d9c7fbdc998f86",
    "sha256:262fbdd19e64e92f21d70027c3d8cc2597b9cdf73c322fc623c5c3f514fc5660",
);
