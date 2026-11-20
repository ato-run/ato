// rustc path_widening.rs && ./path_widening  (run from formal/lean-bugcheck/repro)
#[path = "../../../extensions/providers/nacelle/src/system/path.rs"]
#[allow(dead_code)]
mod path;
fn main() {
    let allowed = vec!["/ato-lean-repro-nonexistent/models".to_string()];
    // Property expected: only paths under the allowed entry are accepted.
    assert!(path::validate_path("/etc/passwd", &allowed).is_err());
    assert!(path::validate_path("/root/.ssh/id_rsa", &allowed).is_err());
    assert!(path::validate_path("/ato-lean-repro-nonexistent/models/a", &allowed).is_ok());
    println!("validate_path rejected paths outside the missing allowlist root with allowed={allowed:?}");
}
