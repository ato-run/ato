use super::*;
use crate::runtime_launch::RuntimeLaunchSpecV1;
use crate::runtime_launch_v2::RuntimeLaunchSpec;

const VOLUME_FIXTURE: &str =
    include_str!("../../tests/fixtures/runtime-launch-spec-v3/service-group-volume.json");
const SEEDED_FIXTURE: &str =
    include_str!("../../tests/fixtures/runtime-launch-spec-v3/service-group-volume-seeded.json");
const V2_FIXTURE: &str =
    include_str!("../../tests/fixtures/runtime-launch-spec-v2/service-group.json");

fn volume() -> RuntimeLaunchSpecV3 {
    RuntimeLaunchSpecV3::parse(VOLUME_FIXTURE).expect("volume fixture is valid")
}

fn refused(mutate: impl FnOnce(&mut RuntimeLaunchSpecV3)) -> RuntimeLaunchSpecError {
    let mut spec = volume();
    mutate(&mut spec);
    spec.validate().expect_err("mutation must be refused")
}

fn backing(spec: &mut RuntimeLaunchSpecV3) -> &mut RunnerVolumeBackingV3 {
    match &mut spec.state_attachments[0].backing {
        StateBackingV3::RunnerVolume(volume) => volume,
    }
}

#[test]
fn canonical_bytes_are_the_fixture_bytes() {
    for (fixture, digest) in [
        (
            VOLUME_FIXTURE,
            "sha256:66083630ebc016f975488316a87a6f714c2e2647245d745e3ff7406243e370ce",
        ),
        (
            SEEDED_FIXTURE,
            "sha256:acb0c5090650de00dfd1e5f9a570aa86d947bbc88ed5a2d2bf105097c2a38ed5",
        ),
    ] {
        let spec = RuntimeLaunchSpecV3::parse(fixture).expect("fixture is valid");
        assert_eq!(
            String::from_utf8(spec.canonical_bytes().unwrap()).unwrap(),
            fixture
        );
        // Pinned in ato-api too: the control plane must produce these bytes.
        assert_eq!(spec.canonical_digest().unwrap(), digest);
    }
}

#[test]
fn the_versioned_parser_keeps_the_sent_digest_and_a_group_view() {
    let parsed = RuntimeLaunchSpec::parse(VOLUME_FIXTURE).unwrap();
    let RuntimeLaunchSpec::V3 { spec, view } = &parsed else {
        panic!("v3 fixture parsed as another version");
    };
    assert_eq!(
        parsed.canonical_digest().unwrap(),
        spec.canonical_digest().unwrap(),
        "the digest is of the v3 bytes, never of the derived view"
    );
    // Group handling sees an ordinary group; the attachment has a fence and
    // no revision to restore.
    assert_eq!(parsed.service_group_spec(), Some(view));
    assert_eq!(parsed.state_attachments()[0].revision_ref, None);
    assert_eq!(parsed.state_attachments()[0].writer_fence, Some(3));
    let volume = parsed.runner_volume("data").expect("data is volume-backed");
    assert_eq!(volume.volume_ref, "svol_01M2SVCGR0VP000000000000V1");
    assert_eq!(parsed.runner_volume("other"), None);
}

#[test]
fn a_revision_backed_group_is_untouched_by_v3() {
    let parsed = RuntimeLaunchSpec::parse(V2_FIXTURE).unwrap();
    assert!(matches!(parsed, RuntimeLaunchSpec::V2(_)));
    assert_eq!(parsed.runner_volume("data"), None);
    assert_eq!(
        parsed.canonical_digest().unwrap(),
        "sha256:0b31af7926415dc9b94027c83af2bb77812fe86f960450e9d55525a8b5afbdb1"
    );
}

#[test]
fn older_parsers_fail_closed_on_a_volume_attachment() {
    // What a Runner without v3 does: refuse by shape before materializing
    // anything, never read the volume as a working copy.
    assert!(RuntimeLaunchSpecV2::parse(VOLUME_FIXTURE).is_err());
    assert!(RuntimeLaunchSpecV1::parse(VOLUME_FIXTURE).is_err());
    // A v2 envelope carrying a v3 attachment is refused too, not reinterpreted.
    let smuggled = VOLUME_FIXTURE.replace(
        RUNTIME_LAUNCH_SPEC_V3_PROTOCOL,
        RUNTIME_LAUNCH_SPEC_V2_PROTOCOL,
    );
    assert_eq!(
        RuntimeLaunchSpec::parse(&smuggled).unwrap_err().code(),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_FORBIDDEN_FIELD"
    );
}

#[test]
fn only_a_strict_volume_ref_is_accepted() {
    for bad in [
        "svol_../../etc/passwd",
        "svol_01M2SVCGR0VP000000000000V",
        "svol_01M2SVCGR0VP000000000000VIX",
        "svol_01m2svcgr0vp000000000000v1",
        "svol_01M2SVCGR0VP00000000000/V1",
        "vol_01M2SVCGR0VP000000000000V1",
        "",
    ] {
        assert!(!is_volume_ref(bad), "{bad}");
        let error = refused(|spec| backing(spec).volume_ref = bad.to_owned());
        assert_eq!(
            error.code(),
            "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_STATE_VOLUME"
        );
    }
    assert!(is_volume_ref("svol_01M2SVCGR0VP000000000000V1"));
}

#[test]
fn volume_invariants_are_refused() {
    let code = |error: RuntimeLaunchSpecError| error.code();
    assert_eq!(
        code(refused(|spec| backing(spec).capacity_bytes = 0)),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_STATE_VOLUME"
    );
    assert_eq!(
        code(refused(|spec| {
            backing(spec).initialize_from_revision_ref = Some("../seed".to_owned())
        })),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_STATE_VOLUME"
    );
    assert_eq!(
        code(refused(
            |spec| spec.state_attachments[0].access = StateAccessV1::ReadOnly
        )),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_STATE_VOLUME"
    );
    // A v3 spec without a volume would be a v2 spec under another name.
    let error = refused(|spec| {
        spec.state_attachments.clear();
        let LaunchRealizationV2::OciServiceGroup(group) = &mut spec.realization;
        for service in &mut group.services {
            service.state_keys.clear();
        }
    });
    assert_eq!(
        code(error),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_STATE_VOLUME"
    );
    // Group rules still apply through the view.
    let error = refused(|spec| {
        let LaunchRealizationV2::OciServiceGroup(group) = &mut spec.realization;
        group.services[1].state_keys = vec!["data".to_owned()];
    });
    assert_eq!(
        code(error),
        "ATO_ERR_RUNTIME_LAUNCH_SPEC_STATE_KEY_CONFLICT"
    );
}

#[test]
fn the_spec_carries_no_host_path() {
    for fixture in [VOLUME_FIXTURE, SEEDED_FIXTURE] {
        for needle in ["/var/", "/home/", "/tmp", "state-volume-root", "volumes/"] {
            assert!(!fixture.contains(needle), "{needle}");
        }
    }
}
