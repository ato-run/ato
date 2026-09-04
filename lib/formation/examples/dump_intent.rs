//! Print the intent and plan a source compiles to, for acceptance comparison.
use ato_formation::detect::{FieldOrigins, detect};
use ato_formation::intent::{AuthoredOverrides, compile_build_plan, compile_intent};
use std::collections::BTreeMap;

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().expect("usage: dump_intent <root> [k=v ...]");
    let overrides = AuthoredOverrides(
        args.filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((key.to_owned(), value.to_owned()))
        })
        .collect::<BTreeMap<_, _>>(),
    );
    let evidence = detect(std::path::Path::new(&root)).expect("detect");
    let mut origins = FieldOrigins::default();
    let intent = compile_intent(&evidence, &overrides, "/app", &mut origins).expect("intent");
    let plan = compile_build_plan(&intent, "/app", "x86_64-unknown-linux-gnu").expect("plan");
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "intent": intent, "plan": plan, "origins": origins
        }))
        .expect("json")
    );
}
