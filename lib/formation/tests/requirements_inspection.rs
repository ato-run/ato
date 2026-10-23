use ato_formation::{
    authoring::{BindingContext, bind},
    capsule_toml::parse_capsule_toml,
    requirements::inspection,
};

#[test]
fn inspection_reuses_d_and_k_without_changing_identity_or_exposing_env_values() {
    let draft = parse_capsule_toml(include_str!("fixtures/proposal-python.toml")).unwrap();
    let (k, mut d) = bind(
        &draft,
        &BindingContext {
            source_closure_ref: &format!("sha256:{}", "a".repeat(64)),
        },
    )
    .unwrap();
    d.steps[0].env.insert(
        "PUBLIC_CONFIG".into(),
        "a-value-that-inspection-must-not-copy".into(),
    );
    let before = (k.contract_ref().unwrap(), d.derivation_ref().unwrap());
    let report = inspection::inspect(&d, &k);
    assert!(std::ptr::eq(report.requirements, &d.requirements));
    assert!(std::ptr::eq(report.verification, &k));
    assert_eq!(report.components.len(), d.steps.len());
    assert!(!report.runtime_verified);
    let json = serde_json::to_string(&report).unwrap();
    assert!(json.contains("PUBLIC_CONFIG"));
    assert!(!json.contains("a-value-that-inspection-must-not-copy"));
    assert_eq!(
        before,
        (k.contract_ref().unwrap(), d.derivation_ref().unwrap())
    );
}
