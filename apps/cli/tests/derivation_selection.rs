//! Multi-Derivation selection for "ato run" (M2 §1).
//!
//! Each test runs the real binary. The interactive screen is exercised through
//! ATO_TEST_ASSUME_TTY=1, which opts a piped stdin into the prompt path; nothing
//! else about a non-interactive Run changes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use assert_cmd::Command as AssertCommand;
use serde_json::Value;

fn repo(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn ato() -> AssertCommand {
    let mut command =
        AssertCommand::cargo_bin("ato").expect("the ato binary is built for integration tests");
    command.timeout(Duration::from_secs(180));
    command.env_remove("ATO_API_URL");
    command.env_remove("ATO_API_TOKEN");
    command
}

fn ato_with_home(home: &Path) -> AssertCommand {
    let mut command = ato();
    command.env("ATO_HOME", home);
    command
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn artifact_digest(path: &Path) -> String {
    ato_portable_application::bundle_sha256(&fs::read(path).expect("read the bundle"))
}

/// The declared routes of a bundle, in its canonical order.
fn declared_derivations(bytes: &[u8]) -> (String, Vec<String>) {
    match ato_objects::decode_capsule_bundle_document(bytes).expect("a decodable bundle") {
        ato_objects::CapsuleBundleDocument::PortableApplicationV3(bundle)
        | ato_objects::CapsuleBundleDocument::PortableApplicationV4(bundle) => {
            (bundle.index.root_contract_ref, bundle.index.derivations)
        }
        other => panic!("the fixtures are portable applications, got {other:?}"),
    }
}

/// "ato run --list-derivations" as a parsed document.
fn list_derivations(home: &Path, capsule: &Path) -> Value {
    let output = ato_with_home(home)
        .arg("run")
        .arg(capsule)
        .arg("--list-derivations")
        .args(["--api-url", "http://127.0.0.1:9"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "listing failed: {}",
        stderr(&output)
    );
    serde_json::from_slice(&output.stdout).expect("the listing is one JSON document")
}

/// The 1-based menu position of the candidate with this realization token.
fn menu_position(candidates: &Value, realization: &str) -> usize {
    candidates["derivations"]
        .as_array()
        .expect("a list")
        .iter()
        .position(|candidate| candidate["realization"] == realization)
        .map(|index| index + 1)
        .unwrap_or_else(|| panic!("no {realization} candidate in {candidates}"))
}

/// One declared route is not a choice: the Run starts it, as it always did.
#[test]
fn a_single_derivation_bundle_runs_without_a_selection() {
    let home = tempfile::tempdir().unwrap();
    let capsule = repo("samples/interop-static.capsule");
    let output = ato_with_home(home.path())
        .arg("run")
        .arg(&capsule)
        .arg("--no-open")
        .args(["--api-url", "http://127.0.0.1:9"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "the single-route Run failed: {}",
        stderr(&output)
    );
    assert!(stdout(&output).contains("Route: "), "{}", stdout(&output));
    assert!(!stdout(&output).contains("Select a derivation"));
    assert!(!stdout(&output).contains("Select one to run"));

    // Listing one route still describes it, in the same document shape.
    let candidates = list_derivations(home.path(), &capsule);
    assert_eq!(
        candidates["derivations"].as_array().expect("a list").len(),
        1
    );
    assert_eq!(candidates["derivations"][0]["realization"], "static_web");
    assert_eq!(candidates["derivations"][0]["runnable"], true);
}

/// A bundle with several routes is never resolved implicitly: the refusal
/// names every candidate, the flag that selects one, and exits 2.
#[test]
fn an_ambiguous_bundle_refuses_with_its_candidates_and_exit_two() {
    let home = tempfile::tempdir().unwrap();
    let capsule = repo("samples/interop-multi-derivation.capsule");
    let bytes = fs::read(&capsule).unwrap();
    let (_, declared) = declared_derivations(&bytes);
    let output = ato_with_home(home.path())
        .arg("run")
        .arg(&capsule)
        .arg("--no-open")
        .args(["--api-url", "http://127.0.0.1:9"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "a missing decision exits 2: {}",
        stderr(&output)
    );
    let message = stderr(&output);
    assert!(message.contains("declares 2 derivations"), "{message}");
    assert!(
        message.contains("select one with --derivation <sha256:...>"),
        "{message}"
    );
    for derivation in &declared {
        assert!(message.contains(derivation), "{message}");
    }
    // Nothing was realized.
    assert!(!stdout(&output).contains("Route: "), "{}", stdout(&output));
    assert!(!stdout(&output).contains("URL: "), "{}", stdout(&output));
}

/// The JSON document a Desktop can consume: the same facts, the same order.
#[test]
fn list_derivations_prints_the_candidates_as_json_in_a_stable_order() {
    let home = tempfile::tempdir().unwrap();
    let capsule = repo("samples/interop-multi-derivation.capsule");
    let bytes = fs::read(&capsule).unwrap();
    let (contract_ref, declared) = declared_derivations(&bytes);

    let candidates = list_derivations(home.path(), &capsule);
    assert_eq!(candidates["schema"], "ato.derivation-candidates/1");
    assert_eq!(candidates["bundle_sha256"], artifact_digest(&capsule));
    assert_eq!(candidates["contract_ref"], contract_ref);
    assert!(candidates["host"]["os"].is_string(), "{candidates}");
    assert!(candidates["host"]["arch"].is_string(), "{candidates}");

    let listed: Vec<String> = candidates["derivations"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|candidate| {
            candidate["derivation_ref"]
                .as_str()
                .expect("a reference")
                .to_owned()
        })
        .collect();
    assert_eq!(listed, declared, "the document keeps the bundle order");

    for candidate in candidates["derivations"].as_array().unwrap() {
        assert!(
            [
                "static_web",
                "process",
                "oci",
                "native_terminal",
                "native_pixel"
            ]
            .contains(&candidate["realization"].as_str().expect("a realization")),
            "{candidate}"
        );
        assert!(candidate["runnable"].is_boolean(), "{candidate}");
        assert!(
            candidate["requires"]["container"].is_boolean(),
            "{candidate}"
        );
        assert!(candidate["permissions"].is_array(), "{candidate}");
        assert!(candidate["tools"].is_array(), "{candidate}");
        assert!(candidate["admission"]["state"].is_string(), "{candidate}");
        assert!(candidate["admission"]["reasons"].is_array(), "{candidate}");
    }

    // A second invocation prints the same order and the same values.
    let again = list_derivations(home.path(), &capsule);
    assert_eq!(again, candidates);
}

/// An explicitly named route runs exactly as before: the bundle bytes and K are
/// untouched, and the receipt names the route that was named.
#[test]
fn an_explicit_derivation_runs_as_before_and_keeps_k_and_the_bundle_digest() {
    let home = tempfile::tempdir().unwrap();
    let evidence = tempfile::tempdir().unwrap();
    let capsule = repo("samples/interop-multi-derivation.capsule");
    let before = artifact_digest(&capsule);
    let (contract_ref, _) = declared_derivations(&fs::read(&capsule).unwrap());
    let candidates = list_derivations(home.path(), &capsule);
    let static_ref = candidates["derivations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["realization"] == "static_web")
        .expect("the static web route")["derivation_ref"]
        .as_str()
        .unwrap()
        .to_owned();
    let receipt_path = evidence.path().join("receipt.json");
    let output = ato_with_home(home.path())
        .arg("run")
        .arg(&capsule)
        .args(["--derivation", &static_ref])
        .arg("--no-open")
        .arg("--verification-receipt")
        .arg(&receipt_path)
        .args(["--api-url", "http://127.0.0.1:9"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "the explicit Run failed: {}",
        stderr(&output)
    );
    assert!(
        stdout(&output).contains(&format!("Route: {static_ref}")),
        "{}",
        stdout(&output)
    );
    let receipt: Value = serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    assert_eq!(receipt["contract_ref"], contract_ref, "K is not rewritten");
    assert_eq!(
        receipt["bundle_sha256"], before,
        "the bundle bytes are reused"
    );
    assert_eq!(receipt["derivation_ref"], static_ref);
    assert_eq!(receipt["fully_satisfied"], true);
    assert_eq!(
        artifact_digest(&capsule),
        before,
        "selecting a route never rewrites the bundle"
    );
    // The declared permissions of the selected route are shown before it runs.
    assert!(
        stderr(&output).contains(&format!("Route {static_ref} requires no host permissions.")),
        "{}",
        stderr(&output)
    );
}

/// A route this host cannot run is refused before anything is realized, in the
/// capability vocabulary -- and the Run is never sent to another route.
#[test]
fn an_unsupported_derivation_is_refused_before_execution_without_a_fallback() {
    let home = tempfile::tempdir().unwrap();
    let evidence = tempfile::tempdir().unwrap();
    let built = tempfile::tempdir().unwrap();
    let bundle_path = built.path().join("native-multi.capsule");
    let packed = ato_with_home(home.path())
        .arg("pack")
        .arg(repo("samples/native-desktop-multi-d"))
        .arg("-o")
        .arg(&bundle_path)
        .output()
        .unwrap();
    assert!(packed.status.success(), "pack failed: {}", stderr(&packed));

    let candidates = list_derivations(home.path(), &bundle_path);
    let windows = candidates["derivations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["native_platform"] == "windows/x86_64")
        .expect("the Windows native route");
    assert_eq!(windows["runnable"], false);
    let windows_ref = windows["derivation_ref"].as_str().unwrap().to_owned();
    let receipt_path = evidence.path().join("receipt.json");
    let output = ato_with_home(home.path())
        .arg("run")
        .arg(&bundle_path)
        .args(["--derivation", &windows_ref])
        .arg("--no-open")
        .arg("--verification-receipt")
        .arg(&receipt_path)
        .args(["--api-url", "http://127.0.0.1:9"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(
        message.contains(&format!(
            "capability derivation_ref={windows_ref} route=Native Desktop"
        )),
        "{message}"
    );
    assert!(
        message.contains("runnable=no state=Unavailable"),
        "{message}"
    );
    assert!(message.contains("capability reason="), "{message}");
    assert!(
        message.contains("derivation-capability-unavailable"),
        "{message}"
    );
    assert!(message.contains("the Run was not started"), "{message}");
    assert!(!receipt_path.exists(), "a refused Run publishes no receipt");
    assert!(
        !stdout(&output).contains("Route: "),
        "no other route was run: {}",
        stdout(&output)
    );
    assert!(!stdout(&output).contains("URL: "), "{}", stdout(&output));
}

/// The numbered screen is answered, and the Run starts exactly that route.
#[test]
fn a_menu_choice_runs_exactly_the_chosen_derivation() {
    let home = tempfile::tempdir().unwrap();
    let evidence = tempfile::tempdir().unwrap();
    let capsule = repo("samples/interop-multi-derivation.capsule");
    let (contract_ref, _) = declared_derivations(&fs::read(&capsule).unwrap());
    let candidates = list_derivations(home.path(), &capsule);
    let position = menu_position(&candidates, "static_web");
    let chosen = candidates["derivations"].as_array().unwrap()[position - 1]["derivation_ref"]
        .as_str()
        .unwrap()
        .to_owned();
    let receipt_path = evidence.path().join("receipt.json");
    let output = ato_with_home(home.path())
        .env("ATO_TEST_ASSUME_TTY", "1")
        .arg("run")
        .arg(&capsule)
        .arg("--no-open")
        .arg("--verification-receipt")
        .arg(&receipt_path)
        .args(["--api-url", "http://127.0.0.1:9"])
        .write_stdin(format!("{position}\n"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "the chosen Run failed: {}",
        stderr(&output)
    );
    let screen = stdout(&output);
    assert!(screen.contains("declares 2 derivations"), "{screen}");
    let count = candidates["derivations"].as_array().unwrap().len();
    assert!(
        screen.contains(&format!("Select a derivation [1-{count}] (q to quit):")),
        "{screen}"
    );
    assert!(stdout(&output).contains(&format!("Route: {chosen}")));
    let receipt: Value = serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    assert_eq!(receipt["derivation_ref"], chosen);
    assert_eq!(receipt["contract_ref"], contract_ref);
    assert_eq!(receipt["fully_satisfied"], true);
}

/// Quitting the screen starts nothing and publishes nothing.
#[test]
fn quitting_the_menu_starts_nothing() {
    let home = tempfile::tempdir().unwrap();
    let evidence = tempfile::tempdir().unwrap();
    let receipt_path = evidence.path().join("receipt.json");
    let output = ato_with_home(home.path())
        .env("ATO_TEST_ASSUME_TTY", "1")
        .arg("run")
        .arg(repo("samples/interop-multi-derivation.capsule"))
        .arg("--no-open")
        .arg("--verification-receipt")
        .arg(&receipt_path)
        .args(["--api-url", "http://127.0.0.1:9"])
        .write_stdin("q\n")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("nothing was started"),
        "{}",
        stderr(&output)
    );
    assert!(!receipt_path.exists());
    assert!(!stdout(&output).contains("Route: "), "{}", stdout(&output));
}

/// A route that declares permissions asks before it runs, and a "no" starts
/// nothing. Skipped when this host cannot run the fixture's native route: the
/// refusal that host produces is covered by the capability test above.
#[test]
fn a_permission_bearing_route_asks_before_it_runs() {
    let home = tempfile::tempdir().unwrap();
    let evidence = tempfile::tempdir().unwrap();
    let built = tempfile::tempdir().unwrap();
    let bundle_path = built.path().join("native-multi.capsule");
    let packed = ato_with_home(home.path())
        .arg("pack")
        .arg(repo("samples/native-desktop-multi-d"))
        .arg("-o")
        .arg(&bundle_path)
        .output()
        .unwrap();
    assert!(packed.status.success(), "pack failed: {}", stderr(&packed));
    let candidates = list_derivations(home.path(), &bundle_path);
    let position = menu_position(&candidates, "native_pixel");
    let native = &candidates["derivations"].as_array().unwrap()[position - 1];
    if native["runnable"] != true {
        eprintln!("SKIP (host capability): this host cannot run the fixture's native route");
        return;
    }
    let receipt_path = evidence.path().join("receipt.json");
    let output = ato_with_home(home.path())
        .env("ATO_TEST_ASSUME_TTY", "1")
        .arg("run")
        .arg(&bundle_path)
        .arg("--no-open")
        .arg("--verification-receipt")
        .arg(&receipt_path)
        .args(["--api-url", "http://127.0.0.1:9"])
        .write_stdin(format!("{position}\nn\n"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("requires:"), "{message}");
    assert!(message.contains("  - native: "), "{message}");
    assert!(message.contains("[y/N]"), "{message}");
    assert!(message.contains("nothing was started"), "{message}");
    assert!(
        !receipt_path.exists(),
        "a declined Run publishes no receipt"
    );
    assert!(!stdout(&output).contains("Route: "), "{}", stdout(&output));
}

/// A read-only listing never turns into a Run, and a computation-root bundle
/// declares no Derivation to list.
#[test]
fn list_derivations_refuses_a_computation_root_bundle() {
    let home = tempfile::tempdir().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("computation-v2.capsule");
    fs::write(
        &path,
        format!(
            "{{\"index\":{{\"objects\":[],\"root\":\"blake3:{}\",\"version\":2}},\"payloads\":[]}}",
            "0".repeat(64)
        ),
    )
    .unwrap();
    let output = ato_with_home(home.path())
        .arg("run")
        .arg(&path)
        .arg("--list-derivations")
        .args(["--api-url", "http://127.0.0.1:9"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("--list-derivations requires a portable application"),
        "{}",
        stderr(&output)
    );
    assert!(!stdout(&output).contains("Route: "), "{}", stdout(&output));
}
