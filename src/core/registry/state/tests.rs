use super::parse_state_reference;

#[test]
fn parse_state_reference_accepts_bare_and_scheme_forms() {
    assert_eq!(parse_state_reference("state-demo"), Some("state-demo"));
    assert_eq!(
        parse_state_reference("ato-state://state-demo"),
        Some("state-demo")
    );
    assert_eq!(parse_state_reference("/absolute/path"), None);
}
