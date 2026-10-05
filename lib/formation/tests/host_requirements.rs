use ato_formation::{
    authoring::{BindingContext, bind},
    capsule_toml::parse_capsule_toml,
    requirements::{
        AcceleratorRequirement, AcceleratorVendor, ExecutionRequirements, HostArch, HostOs,
        HostRequirement,
    },
};

const FIXTURE: &str = include_str!("fixtures/proposal-python.toml");

fn closure() -> String {
    format!("sha256:{}", "a".repeat(64))
}

fn bound(text: &str) -> ato_formation::authoring::BoundDerivation {
    let draft = parse_capsule_toml(text).unwrap();
    bind(
        &draft,
        &BindingContext {
            source_closure_ref: &closure(),
        },
    )
    .unwrap()
    .1
}

fn gpu_host() -> HostRequirement {
    HostRequirement {
        os: HostOs::Linux,
        arch: HostArch::X86_64,
        accelerators: vec![AcceleratorRequirement {
            vendor: AcceleratorVendor::Nvidia,
            count: 1,
            min_vram_mib: 16_000,
        }],
        min_memory_mib: Some(30_000),
        min_scratch_mib: None,
    }
}

#[test]
fn a_route_without_a_host_condition_keeps_its_exact_bytes_and_ref() {
    let d = bound(FIXTURE);
    let json = serde_json::to_value(&d).unwrap();
    assert!(json.get("requirements").is_none());
    // With no host condition the canonical bytes carry no `host` member (and
    // here no `requirements` at all), so this ref is the one this route had
    // before the member existed. Pinned so that no later change moves it.
    assert_eq!(
        d.derivation_ref().unwrap(),
        "sha256:f1fa06d6b516942042f0535e02560d944778ba8e3553d67114341d9e5a0e6a56"
    );
}

#[test]
fn a_host_condition_is_part_of_the_route_not_of_the_contract() {
    let draft = parse_capsule_toml(FIXTURE).unwrap();
    let (k, mut d) = bind(
        &draft,
        &BindingContext {
            source_closure_ref: &closure(),
        },
    )
    .unwrap();
    let (original_k, original_d) = (k.contract_ref().unwrap(), d.derivation_ref().unwrap());
    d.requirements.host = Some(gpu_host());
    assert_ne!(d.derivation_ref().unwrap(), original_d);
    assert_eq!(k.contract_ref().unwrap(), original_k);
    // A host condition alone is still a requirement: it is not dropped from
    // the canonical bytes as if it were empty.
    assert!(!d.requirements.is_empty());
    let json = serde_json::to_value(&d).unwrap();
    assert_eq!(
        json["requirements"]["host"]["accelerators"][0]["vendor"],
        "nvidia"
    );
}

#[test]
fn capsule_toml_authors_a_host_condition_in_canonical_form() {
    let mut text = FIXTURE.to_owned();
    text.push_str(
        "\n[requirements.host]\nos = \"linux\"\narch = \"x86_64\"\nmin_memory_mib = 30000\n\n\
         [[requirements.host.accelerators]]\nvendor = \"nvidia\"\ncount = 1\nmin_vram_mib = 16000\n",
    );
    let d = bound(&text);
    assert_eq!(d.requirements.host, Some(gpu_host()));
    assert!(d.derivation_ref().is_ok());
}

#[test]
fn malformed_or_unknown_host_conditions_are_refused() {
    for host in [
        // No unit-less or zero quantities: an unmeetable condition is not a
        // condition anyone could be admitted against.
        serde_json::json!({"os":"linux","arch":"x86_64","accelerators":[{"vendor":"nvidia","count":0,"min_vram_mib":1}]}),
        serde_json::json!({"os":"linux","arch":"x86_64","accelerators":[{"vendor":"nvidia","count":1,"min_vram_mib":0}]}),
        serde_json::json!({"os":"linux","arch":"x86_64","min_memory_mib":0}),
        serde_json::json!({"os":"linux","arch":"x86_64","accelerators":[
            {"vendor":"nvidia","count":1,"min_vram_mib":1},
            {"vendor":"nvidia","count":2,"min_vram_mib":1}]}),
    ] {
        let requirements: ExecutionRequirements =
            serde_json::from_value(serde_json::json!({ "host": host })).unwrap();
        assert!(requirements.validate().is_err(), "{host}");
    }
    for host in [
        serde_json::json!({"os":"windows","arch":"x86_64"}),
        serde_json::json!({"os":"linux","arch":"x86_64","gpu":true}),
        serde_json::json!({"os":"linux","arch":"x86_64","accelerators":[{"vendor":"amd","count":1,"min_vram_mib":1}]}),
        serde_json::json!({"arch":"x86_64"}),
    ] {
        assert!(
            serde_json::from_value::<ExecutionRequirements>(serde_json::json!({ "host": host }))
                .is_err(),
            "{host}"
        );
    }
}

#[test]
fn a_grant_neither_widens_nor_narrows_a_host_condition() {
    let requirements = ExecutionRequirements {
        host: Some(gpu_host()),
        ..Default::default()
    };
    assert!(
        requirements
            .within(&ExecutionRequirements::default())
            .is_ok()
    );
}
