use ato_formation::{
    authoring::{BindingContext, bind},
    capsule_toml::parse_capsule_toml,
    exploration::FormationConfig,
    requirements::{ExecutionPhase, ExecutionRequirements, NetworkRequirement},
};

fn endpoint(phase: ExecutionPhase, host: &str) -> NetworkRequirement {
    NetworkRequirement {
        phase,
        host: host.into(),
        port: 443,
    }
}

#[test]
fn max_rounds_defaults_to_three_and_rejects_nonpositive_or_fractional_values() {
    assert_eq!(
        serde_json::from_str::<FormationConfig>("{}")
            .unwrap()
            .max_rounds
            .get(),
        3
    );
    for value in ["0", "-1", "1.5", "\"3\"", "4294967296"] {
        assert!(
            serde_json::from_str::<FormationConfig>(&format!("{{\"max_rounds\":{value}}}"))
                .is_err()
        );
    }
    assert_eq!(
        serde_json::from_str::<FormationConfig>("{\"max_rounds\":5}")
            .unwrap()
            .max_rounds
            .get(),
        5
    );
}

#[test]
fn authority_ceiling_is_exact_and_phase_scoped() {
    let ceiling = ExecutionRequirements {
        network: vec![endpoint(ExecutionPhase::Dependencies, "pypi.org")],
        authority: vec![],
        host: None,
    };
    assert!(ceiling.within(&ceiling).is_ok());
    let runtime = ExecutionRequirements {
        network: vec![endpoint(ExecutionPhase::Runtime, "pypi.org")],
        authority: vec![],
        host: None,
    };
    assert_eq!(
        runtime.within(&ceiling).unwrap_err().0,
        "exploration_authority_exceeded"
    );
}

#[test]
fn requirements_change_derivation_digest_without_changing_frozen_contract() {
    let draft = parse_capsule_toml(include_str!("fixtures/proposal-python.toml")).unwrap();
    let (k, mut d) = bind(
        &draft,
        &BindingContext {
            source_closure_ref: &format!("sha256:{}", "a".repeat(64)),
        },
    )
    .unwrap();
    let original_d = d.derivation_ref().unwrap();
    let original_k = k.contract_ref().unwrap();
    assert!(
        serde_json::to_value(&d)
            .unwrap()
            .get("requirements")
            .is_none()
    );
    d.requirements
        .network
        .push(endpoint(ExecutionPhase::Dependencies, "pypi.org"));
    assert_ne!(d.derivation_ref().unwrap(), original_d);
    assert_eq!(k.contract_ref().unwrap(), original_k);
    let dependency_digest = d.derivation_ref().unwrap();
    d.requirements.network[0].phase = ExecutionPhase::Build;
    assert_ne!(d.derivation_ref().unwrap(), dependency_digest);
}

#[test]
fn unsafe_or_ambiguous_endpoint_forms_are_not_requirements() {
    for host in [
        "127.0.0.1",
        "*.pypi.org",
        "PYPI.ORG",
        "pypi.org/",
        "a.localhost",
        "a.local",
        "user:secret@pypi.org",
    ] {
        let requirements = ExecutionRequirements {
            network: vec![endpoint(ExecutionPhase::Build, host)],
            authority: vec![],
            host: None,
        };
        assert!(requirements.validate().is_err(), "{host}");
    }
}

#[test]
fn approvals_and_ceiling_cannot_be_serialized_as_derivation_requirements() {
    assert!(
        serde_json::from_str::<ExecutionRequirements>(
            "{\"network\":[],\"authority\":[],\"approved\":true}"
        )
        .is_err()
    );
    assert!(serde_json::from_str::<ExecutionRequirements>("{\"ceiling\":{}}").is_err());
}

#[test]
fn capsule_requirement_parse_and_canonical_order_agree() {
    let mut text = include_str!("fixtures/proposal-python.toml").to_owned();
    text.push_str("\n[[requirements.network]]\nphase = \"build\"\nhost = \"pypi.org\"\nport = 443\n\n[[requirements.network]]\nphase = \"dependencies\"\nhost = \"files.pythonhosted.org\"\nport = 443\n");
    let draft = parse_capsule_toml(&text).unwrap();
    let (_, d) = bind(
        &draft,
        &BindingContext {
            source_closure_ref: &format!("sha256:{}", "a".repeat(64)),
        },
    )
    .unwrap();
    assert_eq!(
        d.requirements.network[0].phase,
        ExecutionPhase::Dependencies
    );
    assert!(d.derivation_ref().is_ok());
}
