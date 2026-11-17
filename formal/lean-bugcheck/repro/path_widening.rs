// rustc path_widening.rs && ./path_widening  (run from formal/lean-bugcheck/repro)
#[path = "../../../extensions/providers/nacelle/src/system/path.rs"]
#[allow(dead_code)]
mod path;
fn main() {
    let allowed = vec!["/ato-lean-repro-nonexistent/models".to_string()];
    // Property expected: only paths under the allowed entry are accepted.
    assert!(path::validate_path("/etc/passwd", &allowed).is_ok(), "bug not reproduced");
    assert!(path::validate_path("/root/.ssh/id_rsa", &allowed).is_ok(), "bug not reproduced");
    println!("validate_path accepted /etc/passwd and /root/.ssh/id_rsa with allowed={allowed:?}");
}
