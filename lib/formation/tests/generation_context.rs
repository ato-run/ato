#![cfg(feature = "planning")]
use ato_formation::detect;
use ato_formation::generation_context::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn scan(s: &str) -> EntryPointSummary {
    project_python("entry_a", s.as_bytes(), s.len() as u64).unwrap()
}
fn context() -> GenerationContext {
    GenerationContext::new(
        vec![scan("pass\n")],
        project_project(&detect::DetectorEvidence::default(), &BTreeSet::new()),
        vec![],
        vec![],
    )
    .unwrap()
}
fn wire<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap()
}

#[test]
fn python_reports_only_fixed_markers_not_private_names_or_literals() {
    let source = r#"from http.server import BaseHTTPRequestHandler, HTTPServer
import socket, private_import_CANARY
from fastapi import FastAPI
class private_class_CANARY(BaseHTTPRequestHandler):
    def private_func_CANARY(self):
        self.secret = "private_literal_CANARY"
if __name__ == "__main__":
    HTTPServer(("private_hostname_CANARY", 9999), private_class_CANARY).serve_forever()
"#;
    let result = scan(source);
    assert_eq!(result.source_scan, SourceScan::Complete);
    assert_eq!(
        result.imports,
        vec![
            ImportMarker::HttpServer,
            ImportMarker::Socket,
            ImportMarker::Fastapi
        ]
    );
    assert_eq!(result.frameworks, vec![FrameworkMarker::Fastapi]);
    assert_eq!(result.functions, CountBucket::One);
    assert_eq!(result.classes, CountBucket::One);
    assert!(result.main_guard && result.server_listen && result.custom_http_handler);
    let output = wire(&result);
    assert!(!output.contains("CANARY"));
    assert!(!output.contains("9999"));
    assert!(!output.contains("__main__"));
    assert!(output.len() <= MAX_ENTRY_BYTES);
}

#[test]
fn strings_comments_triples_and_fstrings_never_supply_markers() {
    for source in [
        "# import flask; def CANARY(): listen()\npass\n",
        "x = 'import flask; class CANARY(BaseHTTPRequestHandler): listen()'\n",
        "x = \"import flask; def CANARY(): listen()\"\n",
        "x = '''import flask\ndef CANARY():\n class C(BaseHTTPRequestHandler): listen()\n'''\n",
        "x = \"\"\"import flask\ndef CANARY(): listen()\n\"\"\"\n",
        "x = f'import flask {listen()} CANARY'\n",
        "x = rf'''import flask {listen()} CANARY'''\n",
        "x = f\"{f'{listen()} CANARY'}\"\n",
        "x = f\"{call(\"CANARY\", listen())}\"\n",
        "x = f\"{ {'CANARY': listen()} }\"\n",
        "x = r'CANARY\\\' import flask listen()'\n",
        "x = b'CANARY import flask'\n",
        "x = 'if __name__ == \"__main__\":'\n",
        "CANARY_identifier = 1 # \" unterminated comment is not a string\n",
    ] {
        let result = scan(source);
        assert_eq!(result.source_scan, SourceScan::Complete, "{source}");
        assert!(result.imports.is_empty(), "{source}");
        assert_eq!(result.functions, CountBucket::Zero, "{source}");
        assert_eq!(result.classes, CountBucket::Zero, "{source}");
        assert!(
            !result.main_guard && !result.server_listen && !result.custom_http_handler,
            "{source}"
        );
        assert!(!wire(&result).contains("CANARY"));
    }
}

#[test]
fn imports_use_only_module_roots_not_imported_objects_aliases_or_substrings() {
    let result = scan(
        "from private_CANARY import flask\nfrom .flask import thing\nimport private_CANARY as flask\nimport notflask\nimport flask as private_CANARY, uvicorn\n",
    );
    assert_eq!(
        result.imports,
        vec![ImportMarker::Flask, ImportMarker::Uvicorn]
    );
    assert_eq!(
        result.frameworks,
        vec![FrameworkMarker::Flask, FrameworkMarker::Uvicorn]
    );
    assert!(!wire(&result).contains("CANARY"));
}

#[test]
fn malformed_and_partial_sources_discard_all_preceding_markers() {
    for tail in [
        "'CANARY",
        "\"CANARY",
        "'''CANARY",
        "\"\"\"CANARY",
        "f'{CANARY'",
        "f'}'",
        "(CANARY",
        "[)",
        "\0",
        "'CANARY\n'",
    ] {
        let source = format!("import flask\ndef CANARY(): pass\n{tail}");
        let result = scan(&source);
        assert_eq!(result.source_scan, SourceScan::Unavailable, "{tail}");
        assert!(result.imports.is_empty());
        assert_eq!(result.functions, CountBucket::Zero);
        assert!(!wire(&result).contains("CANARY"));
    }
    assert_eq!(
        project_python("a", &[255, 0], 2).unwrap().source_scan,
        SourceScan::Unavailable
    );
    assert_eq!(
        project_python("a", b"import flask", 500)
            .unwrap()
            .source_scan,
        SourceScan::Unavailable
    );
    assert_eq!(
        project_python("a", b"import flask", 2).unwrap().source_scan,
        SourceScan::Unavailable
    );
}

#[test]
fn size_and_count_buckets_are_bounded_and_ignore_oversized_source() {
    let bytes = vec![b'#'; MAX_SOURCE_BYTES];
    assert_eq!(
        project_python("a", &bytes, MAX_SOURCE_BYTES as u64)
            .unwrap()
            .source_scan,
        SourceScan::Complete
    );
    for full_size in [MAX_SOURCE_BYTES as u64 + 1, u64::MAX] {
        let result = project_python("a", b"import flask\nCANARY", full_size).unwrap();
        assert_eq!(result.source_scan, SourceScan::TooLarge);
        assert_eq!(result.size_bucket, SizeBucket::Over64Kib);
        assert!(result.imports.is_empty());
    }
    assert_eq!(scan("").size_bucket, SizeBucket::Empty);
    assert_eq!(
        scan(&"def CANARY(): pass\n".repeat(40)).functions,
        CountBucket::OverSixteen
    );
    assert_eq!(
        scan(&"class CANARY: pass\n".repeat(4)).classes,
        CountBucket::TwoToFour
    );
    assert_eq!(
        scan(&"class CANARY: pass\n".repeat(5)).classes,
        CountBucket::FiveToSixteen
    );
}

#[test]
fn project_projection_drops_all_detector_content() {
    let evidence = detect::DetectorEvidence {
        present_files: vec!["private_CANARY/path".into(), "pyproject.toml".into()],
        python: Some(detect::PythonEvidence {
            has_uv_lock: true,
            python_version_file: Some("CANARY".into()),
            requires_python: Some("CANARY".into()),
            top_level_modules: vec!["CANARY.py".into()],
            ..Default::default()
        }),
        node: Some(detect::NodeEvidence {
            node_version_file: Some("CANARY".into()),
            package_manager: Some("CANARY".into()),
            script_build: Some("CANARY".into()),
            dependency_names: vec!["CANARY".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    let files = BTreeSet::from(["README.md".into(), "private_CANARY/a.py".into()]);
    let summary = project_project(&evidence, &files);
    assert!(
        summary.python && summary.node && summary.manifest && summary.lockfile && summary.readme
    );
    assert_eq!(summary.python_files, CountBucket::One);
    assert_eq!(summary.regular_files, CountBucket::TwoToFour);
    assert!(!wire(&summary).contains("CANARY"));
}

#[test]
fn projections_keep_last_typed_records_and_never_echo_raw_evidence() {
    let mut failures: Vec<_> = (0..20).map(|_| json!({"status":"fail", "failure_code":"private_CANARY", "message":"CANARY", "runtime_id":"CANARY"})).collect();
    failures.push(json!({"status":"unknown", "failure_code":"http_status_mismatch"}));
    failures.push(json!({"status":"private_CANARY", "failure_code":"CANARY"}));
    let output = project_failures(&json!(failures));
    assert_eq!(output.len(), MAX_FAILURES);
    assert_eq!(output.last().unwrap().status, FailureStatus::Unknown);
    assert_eq!(
        output.last().unwrap().code,
        EvidenceCode::HttpStatusMismatch
    );
    assert_eq!(output[0].code, EvidenceCode::Other);
    assert!(!wire(&output).contains("CANARY"));
    let inspections = json!([
        {"kind":"CANARY", "result":{"CANARY":"CANARY"}},
        {"kind":"candidate_refusals", "target_ref":"CANARY", "result":{"refusals":[{"runtime_id":"CANARY", "reasons":[{"code":"CANARY", "message":"CANARY"},{"code":"network_denied"}]}]}},
        {"kind":"attempt_failures", "result":{"failures":failures}}
    ]);
    let output = project_inspections(&inspections);
    assert_eq!(output.len(), 2);
    assert_eq!(
        output[0].reasons,
        vec![EvidenceCode::Other, EvidenceCode::NetworkDenied]
    );
    assert_eq!(output[1].failures.len(), MAX_FAILURES);
    assert!(!wire(&output).contains("CANARY"));
    let many = json!(vec![inspections[1].clone(); 20]);
    assert_eq!(project_inspections(&many).len(), MAX_INSPECTIONS);
    let reasons = json!([{"kind":"candidate_refusals", "result":{"refusals":[{"reasons":[
        {"code":"network_denied"}, {"code":"timeout"}, {"code":"timeout"}, {"code":"timeout"},
        {"code":"timeout"}, {"code":"timeout"}, {"code":"timeout"}, {"code":"timeout"}, {"code":"timeout"}
    ]}]}}]);
    assert_eq!(
        project_inspections(&reasons)[0].reasons,
        vec![EvidenceCode::Timeout]
    );
}

#[test]
fn strict_serde_rejects_unknown_fields_enums_ids_counts_and_noncanonical_order() {
    let base = serde_json::to_value(context()).unwrap();
    let mut attacks = vec![];
    for id in [
        "",
        "none",
        "a-b",
        "a/b",
        "é",
        "abcdefghijklmnopqrstuvwxyz1234567",
    ] {
        let mut v = base.clone();
        v["entrypoints"][0]["id"] = id.into();
        attacks.push(v);
        assert!(project_python(id, b"", 0).is_err());
    }
    for (key, value) in [
        ("private_path", json!("CANARY")),
        ("language", json!("shell")),
        ("imports", json!(["CANARY"])),
        ("functions", json!(999)),
    ] {
        let mut v = base.clone();
        v["entrypoints"][0][key] = value;
        attacks.push(v);
    }
    for (key, value) in [
        ("schema", json!("CANARY")),
        ("raw_source", json!("CANARY")),
        (
            "entrypoints",
            json!(vec![base["entrypoints"][0].clone(); 17]),
        ),
        (
            "failures",
            json!(vec![json!({"status":"fail","code":"other"}); 17]),
        ),
        (
            "inspections",
            json!(vec![
                json!({"kind":"candidate_refusals","reasons":[],"failures":[]});
                5
            ]),
        ),
    ] {
        let mut v = base.clone();
        v[key] = value;
        attacks.push(v);
    }
    let mut v = base.clone();
    v["entrypoints"][0]["imports"] = json!(["flask", "flask"]);
    attacks.push(v);
    let mut v = base.clone();
    v["entrypoints"][0]["source_scan"] = json!("too_large");
    attacks.push(v);
    let mut v = base.clone();
    v["entrypoints"][0]["source_scan"] = json!("unavailable");
    v["entrypoints"][0]["main_guard"] = json!(true);
    attacks.push(v);
    for attack in attacks {
        assert!(serde_json::from_value::<GenerationContext>(attack).is_err());
    }
    assert!(GenerationContext::from_json(&vec![b' '; MAX_CONTEXT_BYTES + 1]).is_err());
    let mut value = base;
    value["entrypoints"] = json!([scan("pass"), scan("pass")]);
    assert!(serde_json::from_value::<GenerationContext>(value).is_err());
}

#[test]
fn context_order_roundtrip_and_wire_bounds_are_deterministic() {
    let mut entries = vec![];
    let source = "import flask, fastapi, uvicorn, aiohttp, tornado, django, http.server, socketserver, socket, asyncio, wsgiref\ndef CANARY(): pass\nclass CANARY(BaseHTTPRequestHandler): pass\nif __name__ == '__main__': listen()\n";
    for i in (0..16).rev() {
        entries.push(
            project_python(
                &format!("entry_{i:02}"),
                source.as_bytes(),
                source.len() as u64,
            )
            .unwrap(),
        );
    }
    let original = context();
    let c = GenerationContext::new(
        entries.clone(),
        original.project_summary.clone(),
        vec![
            FailureSummary {
                status: FailureStatus::Fail,
                code: EvidenceCode::SearchTransferBudgetExceeded
            };
            16
        ],
        vec![
            InspectionSummary {
                kind: InspectionKind::AttemptFailures,
                reasons: vec![],
                failures: vec![
                    FailureSummary {
                        status: FailureStatus::Inconclusive,
                        code: EvidenceCode::SearchExpandedBudgetExceeded
                    };
                    16
                ]
            };
            4
        ],
    )
    .unwrap();
    assert!(wire(&c).len() <= MAX_CONTEXT_BYTES);
    for entry in &c.entrypoints {
        assert!(wire(entry).len() <= MAX_ENTRY_BYTES);
    }
    entries.reverse();
    let c2 = GenerationContext::new(
        entries,
        c.project_summary.clone(),
        c.failures.clone(),
        c.inspections.clone(),
    )
    .unwrap();
    assert_eq!(wire(&c), wire(&c2));
    assert_eq!(
        GenerationContext::from_json(wire(&c).as_bytes()).unwrap(),
        c
    );
    assert!(c.entrypoints[0].id < c.entrypoints[1].id);
}

#[test]
fn entry_and_inspection_standalone_deserialization_is_also_checked() {
    let mut v = serde_json::to_value(scan("pass")).unwrap();
    v["imports"] = json!(vec!["flask"; 1000]);
    assert!(serde_json::from_value::<EntryPointSummary>(v).is_err());
    for value in [
        json!({"kind":"candidate_refusals","reasons":vec!["other";9],"failures":[]}),
        json!({"kind":"attempt_failures","reasons":["other"],"failures":[]}),
        json!({"kind":"candidate_refusals","reasons":[],"failures":[],"CANARY":"CANARY"}),
    ] {
        assert!(serde_json::from_value::<InspectionSummary>(value).is_err());
    }
    assert_eq!(project_failures(&Value::Null), vec![]);
    assert_eq!(project_inspections(&Value::Null), vec![]);
}

#[test]
fn flat_api_inspections_match_durable_projection_without_merging_payloads() {
    let flat = json!([
        {"kind":"candidate_refusals", "refusals":[{"runtime_id":"CANARY", "reasons":[{"code":"requirement_unmet", "fact":"CANARY"},{"code":"runtime_offline", "last_seen":"CANARY"}]}]},
        {"kind":"attempt_failures", "failures":[{"status":"fail", "failure_code":"http_status_mismatch", "message":"CANARY"}]}
    ]);
    let durable = Value::Array(
        flat.as_array()
            .unwrap()
            .iter()
            .map(|item| json!({"kind":item["kind"], "result":item}))
            .collect(),
    );
    let result = project_inspections(&flat);
    assert_eq!(result, project_inspections(&durable));
    assert_eq!(result.len(), 2);
    assert_eq!(
        result[0].reasons,
        vec![EvidenceCode::RuntimeOffline, EvidenceCode::RequirementUnmet]
    );
    assert_eq!(result[1].failures[0].code, EvidenceCode::HttpStatusMismatch);
    assert!(!wire(&result).contains("CANARY"));
    let ambiguous = json!([{"kind":"attempt_failures", "result":{"failures":[]}, "failures":[{"status":"fail", "failure_code":"timeout"}]}]);
    assert!(project_inspections(&ambiguous)[0].failures.is_empty());
}

#[test]
fn lexical_markers_do_not_confuse_definitions_with_calls_or_class_names_with_bases() {
    let result = scan(
        "def listen(): pass\ndef HTTPServer(): pass\nclass BaseHTTPRequestHandler(object): pass\nclass HTTPServer(object): pass\n",
    );
    assert_eq!(result.source_scan, SourceScan::Complete);
    assert!(!result.server_listen);
    assert!(!result.custom_http_handler);
    let result = scan("class Handler(http.server.BaseHTTPRequestHandler): pass\nserver.listen()\n");
    assert!(result.server_listen && result.custom_http_handler);
    assert!(!scan("custom_listen_CANARY()\n").server_listen);
    assert!(scan("if '__main__' == __name__: pass\n").main_guard);
}
