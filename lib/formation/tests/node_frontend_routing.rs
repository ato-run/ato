//! L0–L7: which Node frontend a source's declarations select. Restores the npm
//! v1 route #1440 took from sources that also carry a pnpm/Yarn/Bun lock.
use ato_formation::{
    authoring::{BindingContext, bind},
    detect::{DetectorEvidence, detect},
    preset::{AppPreset, candidate_authoring, routes_to_node_static_v2, synthesize_authoring},
};
use serde_json::{Value, json};

fn source(package: Value, locks: &[&str]) -> (tempfile::TempDir, DetectorEvidence) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), package.to_string()).unwrap();
    for lock in locks {
        std::fs::write(dir.path().join(lock), "").unwrap();
    }
    std::fs::write(dir.path().join("index.html"), "<html></html>").unwrap();
    let facts = detect(dir.path()).unwrap();
    (dir, facts)
}
fn package(manager: Option<&str>, dev: Option<Value>) -> Value {
    let mut p = json!({"name":"app","scripts":{"build":"vite build","preview":"vite preview"}});
    if let Some(m) = manager {
        p["packageManager"] = json!(m);
    }
    if let Some(d) = dev {
        p["devEngines"] = json!({ "packageManager": d });
    }
    p
}
fn routes_v2(facts: &DetectorEvidence) -> bool {
    routes_to_node_static_v2(facts.node.as_ref().unwrap())
}
/// Byte-identical to the historical node-static/v1 (+ static-files) drafts.
fn assert_v1(facts: &DetectorEvidence) {
    assert!(!routes_v2(facts));
    let ctx = BindingContext {
        source_closure_ref: "sha256:aaaaaaaa",
    };
    let drafts = candidate_authoring(facts).unwrap();
    let expected = [AppPreset::NodeStatic, AppPreset::StaticFiles].map(synthesize_authoring);
    assert_eq!(drafts.len(), expected.len());
    for (draft, old) in drafts.iter().zip(&expected) {
        let (k0, d0) = bind(old, &ctx).unwrap();
        let (k1, d1) = bind(draft, &ctx).unwrap();
        assert_eq!(
            serde_jcs::to_vec(&k0).unwrap(),
            serde_jcs::to_vec(&k1).unwrap()
        );
        assert_eq!(
            serde_jcs::to_vec(&d0).unwrap(),
            serde_jcs::to_vec(&d1).unwrap()
        );
    }
}
fn refused(facts: &DetectorEvidence) -> &'static str {
    assert!(routes_v2(facts));
    candidate_authoring(facts).unwrap_err().code
}

#[test]
fn l0_npm_lock_only_is_byte_identical_v1() {
    assert_v1(&source(package(None, None), &["package-lock.json"]).1);
}
#[test]
fn l1_npm_lock_plus_bun_lock_without_declaration_is_v1() {
    assert_v1(&source(package(None, None), &["package-lock.json", "bun.lock"]).1);
    assert_v1(&source(package(None, None), &["package-lock.json", "bun.lockb"]).1);
}
#[test]
fn l2_npm_lock_plus_bun_lock_with_npm_declaration_is_v1() {
    for (manager, dev) in [
        (Some("npm@10.8.2"), None),
        (None, Some(json!({"name":"npm","version":"10.8.2"}))),
        (
            Some("npm@10.8.2"),
            Some(json!({"name":"npm","version":"10.8.2"})),
        ),
    ] {
        assert_v1(&source(package(manager, dev), &["package-lock.json", "bun.lock"]).1);
    }
}
#[test]
fn l3_bun_declaration_is_deferred_even_with_npm_lock() {
    let (_d, facts) = source(
        package(Some("bun@1.2.0"), None),
        &["package-lock.json", "bun.lock"],
    );
    assert_eq!(refused(&facts), "preset_node_static_v2_bun_deferred");
}
#[test]
fn l4_npm_lock_plus_pnpm_lock_without_declaration_is_historical_v1() {
    assert_v1(
        &source(
            package(None, None),
            &["package-lock.json", "pnpm-lock.yaml"],
        )
        .1,
    );
    assert_v1(&source(package(None, None), &["npm-shrinkwrap.json", "yarn.lock"]).1);
}
#[test]
fn l5_pnpm_declaration_with_npm_lock_is_a_v2_conflict() {
    let (_d, facts) = source(
        package(Some("pnpm@9.15.4"), None),
        &["package-lock.json", "pnpm-lock.yaml"],
    );
    assert_eq!(refused(&facts), "preset_node_static_v2_lock_conflict");
}
#[test]
fn l6_no_npm_lock_with_bun_lock_is_v2_bun_deferred() {
    let (_d, facts) = source(package(None, None), &["bun.lock"]);
    assert_eq!(refused(&facts), "preset_node_static_v2_bun_deferred");
    // pnpm/Yarn lock-only evidence still routes to v2 (6b-A behaviour kept).
    let (_d, facts) = source(package(None, None), &["pnpm-lock.yaml"]);
    assert!(routes_v2(&facts));
}
#[test]
fn l7_conflicting_or_malformed_declarations_never_fall_back_to_v1() {
    for (manager, dev) in [
        (
            Some("npm@10.8.2"),
            Some(json!({"name":"pnpm","version":"9.15.4"})),
        ),
        (
            Some("pnpm@9.15.4"),
            Some(json!({"name":"npm","version":"10.8.2"})),
        ),
        (Some("npm@10.8.2"), Some(json!({"version":"10.8.2"}))),
    ] {
        let (_d, facts) = source(package(manager, dev), &["package-lock.json"]);
        assert!(refused(&facts).starts_with("preset_node_static_v2_"));
    }
}
