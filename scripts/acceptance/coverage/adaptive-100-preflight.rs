//! Read-only 6b-F source/K/catalog projection. This is a measurement harness.
//! The source, compiler, Runtime, validator and provider implementations are unchanged.
use anyhow::{Result, ensure};
use ato_formation::{authoring::{bind, BindingContext}, detect::detect,
    preset::{candidate_authoring, synthesize_authoring, AppPreset}, proposal::*,
    source::{DownloadedArchive, SourceLimits}, workspace::inventory};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf};

fn main() -> Result<()> {
    let a: Vec<_> = std::env::args().collect();
    ensure!(a.len() == 5, "archive digest scratch result.json");
    let verified = DownloadedArchive::new(std::fs::read(&a[1])?)
        .verify_archive_digest(&a[2])?.verify_tree_digest(None, SourceLimits::default())?;
    let closure = verified.closure_ref("")?.as_str().to_owned();
    let tree = verified.materialize(&PathBuf::from(&a[3]).join("tree"), "", SourceLimits::default())?;
    // User-authorized, unchanged preset K for every source. No attempt observations used.
    let (k, _) = bind(&synthesize_authoring(AppPreset::StaticFiles),
        &BindingContext { source_closure_ref: &closure })?;
    let k_ref = k.contract_ref()?;
    let evidence = detect(&tree)?;
    let known = match candidate_authoring(&evidence) {
        Ok(drafts) => drafts.into_iter().map(|draft| {
            match bind(&draft, &BindingContext { source_closure_ref: &closure }) {
                Ok((seen_k, d)) => Ok(json!({"contract_ref":seen_k.contract_ref()?,
                    "same_K":seen_k == k,"derivation_ref":d.derivation_ref()?,
                    "derivation":d})),
                Err(e) => Ok(json!({"binding_error":e.code()})),
            }
        }).collect::<Result<Vec<_>>>()?,
        Err(e) => vec![json!({"preset_error":e.code})],
    };
    let mut paths: Vec<String> = std::fs::read_dir(&tree)?.filter_map(|entry| {
        let e = entry.ok()?;
        if !e.file_type().ok()?.is_file() { return None; }
        let name = e.file_name().into_string().ok()?;
        // Global, source-only rule; no application-specific filename or setup inference.
        (name.ends_with(".py") && name.len() <= 256 && name.as_bytes().first()?.is_ascii_alphanumeric()
          && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
          && !name.contains("..")).then_some(name)
    }).collect();
    paths.sort();
    let total = paths.len();
    paths.truncate(16);
    let entries: BTreeMap<String,String> = paths.into_iter().enumerate()
        .map(|(i,p)| (format!("e_{i:02}"),p)).collect();
    let workspace_facts = inventory(&tree);
    let workspace = workspace_facts.as_ref().ok().and_then(|f| f.authorization());
    let auth = if entries.is_empty() && workspace.is_none() { None } else { Some(ProposalAuthorization {
        execution_plan: None,
        modifiable_derivation_refs:vec![],
        source_domain:SourceDomain { entrypoints:entries, modules:BTreeMap::new() },
        python_http_process:(total > 0).then(|| PythonHttpProcess {
            python_version:"3.12.7".into(), http_port:"app.http".into(), guest_port:8000 }),
        node_static_workspace:workspace,
        policy:CandidateProducerPolicy { max_proposal_rounds:1,max_proposals:1,timeout_ms:30000,
            allow_source_text:true,max_source_bytes:16384 },
    }) };
    let catalog = auth.as_ref().map(|x| x.catalog()).transpose()?;
    let record = json!({"schema":"ato.formation-adaptive-100-source-projection/1",
        "archive_digest":a[2],"closure_ref":closure,"contract":k,"contract_ref":k_ref,
        "known":known,"root_python_files_total":total,"authorization":auth,"operation_catalog":catalog,
        "workspace_inventory":match workspace_facts { Ok(f) => json!(f), Err(e) => json!({"refused":e.code()}) },
        "model_calls":0,"source_programs_executed":0});
    std::fs::write(&a[4],serde_json::to_vec_pretty(&record)?)?;
    Ok(())
}
