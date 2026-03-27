use anyhow::{Context, Result};
use capsule_core::input_resolver::{
    resolve_authoritative_input, ResolveInputOptions, ResolvedInput,
};
use std::fs;
use std::path::{Path, PathBuf};

use super::detect::{self, DetectedProject, NodePackageManager, ProjectType};
use super::recipe::{self, ProjectInfo};
use super::PromptArgs;

mod frameworks;

pub fn execute(
    args: PromptArgs,
    _reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    let project_dir = args
        .path
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .context("Failed to resolve project directory")?;

    match resolve_authoritative_input(&project_dir, ResolveInputOptions::default()) {
        Ok(ResolvedInput::CanonicalLock { canonical, .. }) => {
            anyhow::bail!(
                "{} already exists at {}. `ato init` prompt generation applies to source-only projects in this migration stage.",
                capsule_core::input_resolver::ATO_LOCK_FILE_NAME,
                canonical.path.display()
            );
        }
        Ok(ResolvedInput::CompatibilityProject { project, .. }) => {
            anyhow::bail!(
                "capsule.toml already exists at {}. Prompt generation is intended for source-only projects.",
                project.manifest.path.display()
            );
        }
        Ok(ResolvedInput::SourceOnly { .. }) => {}
        Err(error)
            if error
                .to_string()
                .contains("is not an authoritative command-entry input") =>
        {
            return Err(error.into());
        }
        Err(_) => {}
    }

    let detected = detect::detect_project(&project_dir)?;
    let info = recipe::project_info_from_detection(&detected)?;
    let context = PromptContext::from_project(&project_dir, &detected, &info)?;
    let prompt = render_prompt(&context);

    println!("Analyzing project...");
    println!("Found: {}", context.summary_line());
    if let Some(frameworks) = context.framework_hints_line() {
        println!("Framework hints: {frameworks}");
    }
    if let Some(ambiguity) = context.ambiguities.first() {
        println!("Ambiguity detected: {ambiguity}");
    }
    println!();
    println!("✨ Generated an agent-ready prompt for capsule.toml creation.");
    println!(
        "Copy the prompt below into your preferred AI agent, then validate the result with `ato validate capsule.toml`."
    );
    println!();
    println!("==================================================");
    println!("{prompt}");
    println!("==================================================");

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FrameworkMatch {
    pub name: &'static str,
    pub category: &'static str,
    pub confidence: u8,
}

#[derive(Debug, Clone)]
pub(super) struct PromptInputs {
    pub dir: PathBuf,
    pub detected: DetectedProject,
    pub info: ProjectInfo,
    pub package_json: Option<serde_json::Value>,
    pub cargo_toml: Option<toml::Value>,
    pub pyproject_toml: Option<toml::Value>,
    pub requirements_txt: Option<String>,
    pub go_mod: Option<String>,
}

impl PromptInputs {
    fn load(dir: &Path, detected: &DetectedProject, info: &ProjectInfo) -> Result<Self> {
        Ok(Self {
            dir: dir.to_path_buf(),
            detected: detected.clone(),
            info: info.clone(),
            package_json: read_optional_json(&dir.join("package.json")),
            cargo_toml: read_optional_toml(&dir.join("Cargo.toml")),
            pyproject_toml: read_optional_toml(&dir.join("pyproject.toml")),
            requirements_txt: read_optional_text(&dir.join("requirements.txt")),
            go_mod: read_optional_text(&dir.join("go.mod")),
        })
    }
}

#[derive(Debug, Default)]
struct PromptContext {
    project_dir: PathBuf,
    detected: Option<DetectedProject>,
    info: Option<ProjectInfo>,
    framework_matches: Vec<FrameworkMatch>,
    evidence_files: Vec<String>,
    config_files: Vec<String>,
    output_dirs: Vec<String>,
    artifact_candidates: Vec<String>,
    candidate_entry_files: Vec<String>,
    runtime_metadata: Vec<String>,
    ambiguities: Vec<String>,
    schema_constraints: Vec<String>,
    decision_rules: Vec<String>,
}

impl PromptContext {
    fn from_project(dir: &Path, detected: &DetectedProject, info: &ProjectInfo) -> Result<Self> {
        let inputs = PromptInputs::load(dir, detected, info)?;
        let mut context = Self {
            project_dir: dir.to_path_buf(),
            detected: Some(detected.clone()),
            info: Some(info.clone()),
            ..Self::default()
        };

        context.collect_base_facts(&inputs)?;

        let matched_rules = frameworks::match_rules(&inputs)?;
        context.framework_matches = matched_rules.iter().map(|rule| rule.framework).collect();

        for rule in &matched_rules {
            (rule.add_facts)(&inputs, &mut context)?;
        }

        let mut ambiguities = Vec::new();
        add_generic_ambiguities(&inputs, &context, &mut ambiguities);
        for rule in &matched_rules {
            (rule.add_ambiguities)(&inputs, &context, &mut ambiguities)?;
        }
        context.ambiguities = dedup_preserve_order(ambiguities);

        let mut schema_constraints = vec![
            "Generate a valid `capsule.toml` for Ato `schema_version = \"0.2\"`.".to_string(),
            "Use `type = \"app\"` and include a valid `default_target` plus the matching `[targets.<name>]` table.".to_string(),
            "For source-executed apps, prefer `runtime = \"source\"` and set `entrypoint` to the executable with extra arguments in `cmd = [...]`.".to_string(),
            "Do not invent unsupported fields; if a required field is unclear, ask the user before generating TOML.".to_string(),
        ];
        for rule in &matched_rules {
            (rule.add_schema_constraints)(&context, &mut schema_constraints);
        }
        context.schema_constraints = dedup_preserve_order(schema_constraints);

        let mut decision_rules = Vec::new();
        if !context.has_native_framework() && !context.release_command().is_empty() {
            decision_rules.push(format!(
                "Prefer the detected release command unless the user says it should be different: `{}`.",
                context.release_command()
            ));
        }
        for rule in &matched_rules {
            (rule.add_decision_rules)(&context, &mut decision_rules);
        }
        context.decision_rules = dedup_preserve_order(decision_rules);

        Ok(context)
    }

    fn collect_base_facts(&mut self, inputs: &PromptInputs) -> Result<()> {
        for path in [
            "package.json",
            "package-lock.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "bun.lockb",
            "Cargo.toml",
            "requirements.txt",
            "pyproject.toml",
            "go.mod",
            "Gemfile",
            "src-tauri/Cargo.toml",
            "tauri.conf.json",
            "src-tauri/tauri.conf.json",
            "manage.py",
        ] {
            self.add_existing_file(&inputs.dir, path, FactKind::EvidenceFile);
        }

        for path in [
            "next.config.js",
            "next.config.mjs",
            "next.config.cjs",
            "next.config.ts",
            "vite.config.js",
            "vite.config.mjs",
            "vite.config.cjs",
            "vite.config.ts",
            "astro.config.mjs",
            "astro.config.js",
            "astro.config.ts",
            "nuxt.config.ts",
            "nuxt.config.js",
            "svelte.config.js",
            "svelte.config.ts",
            "electron-builder.json",
            "electron-builder.yml",
            "electron-builder.yaml",
            "forge.config.js",
            "forge.config.ts",
        ] {
            self.add_existing_file(&inputs.dir, path, FactKind::ConfigFile);
        }

        for path in [
            "src",
            "app",
            "pages",
            "public",
            "dist",
            "build",
            "out",
            ".next",
            ".output",
            "release",
            "target",
            "src-tauri",
            "src-tauri/target",
            "cmd",
            "src/bin",
        ] {
            self.add_existing_dir(&inputs.dir, path, FactKind::OutputDir);
        }

        for path in [
            "main.py",
            "app.py",
            "server.py",
            "manage.py",
            "main.go",
            "index.js",
            "server.js",
            "app.js",
            "src/index.ts",
            "src/main.ts",
            "src/index.js",
            "src/main.js",
            "src/main.rs",
        ] {
            self.add_existing_file(&inputs.dir, path, FactKind::CandidateEntryFile);
        }

        if inputs.dir.join("src/bin").exists() {
            for entry in fs::read_dir(inputs.dir.join("src/bin"))? {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    self.add_candidate_entry_file(relative_display(&inputs.dir, &entry.path()));
                }
            }
        }

        if inputs.dir.join("cmd").exists() {
            for entry in fs::read_dir(inputs.dir.join("cmd"))? {
                let entry = entry?;
                self.add_candidate_entry_file(relative_display(&inputs.dir, &entry.path()));
            }
        }

        if let Some(node) = inputs.detected.node.as_ref() {
            self.add_runtime_metadata(format!(
                "Node package manager: {}",
                node_package_manager_label(node.package_manager)
            ));
            let mut scripts = Vec::new();
            if node.scripts.has_dev {
                scripts.push("dev");
            }
            if node.scripts.has_build {
                scripts.push("build");
            }
            if node.scripts.has_start {
                scripts.push("start");
            }
            if !scripts.is_empty() {
                self.add_runtime_metadata(format!("Declared Node scripts: {}", scripts.join(", ")));
            }
        }

        if let Some(package_json) = inputs.package_json.as_ref() {
            if let Some(main) = package_json.get("main").and_then(|value| value.as_str()) {
                self.add_candidate_entry_file(main.to_string());
                self.add_runtime_metadata(format!("package.json main: `{main}`"));
            }
            if let Some(name) = package_json.get("name").and_then(|value| value.as_str()) {
                self.add_runtime_metadata(format!("package.json name: `{name}`"));
            }
        }

        if let Some(dev) = inputs.info.node_dev_entrypoint.as_ref() {
            self.add_runtime_metadata(format!("Suggested dev command: `{}`", dev.join(" ")));
        }
        if let Some(release) = inputs.info.node_release_entrypoint.as_ref() {
            self.add_runtime_metadata(format!(
                "Suggested release command: `{}`",
                release.join(" ")
            ));
        } else if !inputs.info.entrypoint.is_empty() {
            self.add_runtime_metadata(format!(
                "Suggested entry command: `{}`",
                inputs.info.entrypoint.join(" ")
            ));
        }

        collect_artifact_candidates(&inputs.dir, self)?;

        Ok(())
    }

    fn summary_line(&self) -> String {
        let mut parts = vec![self.detected().project_type.as_str().to_string()];
        if !self.evidence_files.is_empty() {
            parts.push(format!("evidence: {}", self.evidence_files.join(", ")));
        }
        if !self.framework_matches.is_empty() {
            parts.push(format!(
                "frameworks: {}",
                self.framework_matches
                    .iter()
                    .map(|item| item.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        parts.join(" | ")
    }

    fn framework_hints_line(&self) -> Option<String> {
        if self.framework_matches.is_empty() {
            None
        } else {
            Some(
                self.framework_matches
                    .iter()
                    .map(|item| item.name)
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        }
    }

    fn detected(&self) -> &DetectedProject {
        self.detected
            .as_ref()
            .expect("detected project must be set")
    }

    fn info(&self) -> &ProjectInfo {
        self.info.as_ref().expect("project info must be set")
    }

    fn has_framework(&self, name: &str) -> bool {
        self.framework_matches.iter().any(|item| item.name == name)
    }

    fn has_native_framework(&self) -> bool {
        self.has_framework("Tauri") || self.has_framework("Electron")
    }

    fn has_output_dir(&self, path: &str) -> bool {
        self.output_dirs.iter().any(|item| item == path)
    }

    fn has_artifact_suffix(&self, suffix: &str) -> bool {
        self.artifact_candidates
            .iter()
            .any(|item| item.ends_with(suffix))
    }

    fn release_command(&self) -> String {
        if let Some(release) = self.info().node_release_entrypoint.as_ref() {
            release.join(" ")
        } else {
            self.info().entrypoint.join(" ")
        }
    }

    fn add_existing_file(&mut self, dir: &Path, relative: &str, kind: FactKind) {
        let path = dir.join(relative);
        if path.is_file() {
            self.add_fact(kind, relative.to_string());
        }
    }

    fn add_existing_dir(&mut self, dir: &Path, relative: &str, kind: FactKind) {
        let path = dir.join(relative);
        if path.is_dir() {
            self.add_fact(kind, relative.to_string());
        }
    }

    fn add_evidence_file(&mut self, path: impl Into<String>) {
        self.add_fact(FactKind::EvidenceFile, path.into());
    }

    fn add_config_file(&mut self, path: impl Into<String>) {
        self.add_fact(FactKind::ConfigFile, path.into());
    }

    fn add_output_dir(&mut self, path: impl Into<String>) {
        self.add_fact(FactKind::OutputDir, path.into());
    }

    fn add_artifact_candidate(&mut self, path: impl Into<String>) {
        self.add_fact(FactKind::ArtifactCandidate, path.into());
    }

    fn add_candidate_entry_file(&mut self, path: impl Into<String>) {
        self.add_fact(FactKind::CandidateEntryFile, path.into());
    }

    fn add_runtime_metadata(&mut self, value: impl Into<String>) {
        self.add_fact(FactKind::RuntimeMetadata, value.into());
    }

    fn add_fact(&mut self, kind: FactKind, value: String) {
        if value.trim().is_empty() {
            return;
        }
        let target = match kind {
            FactKind::EvidenceFile => &mut self.evidence_files,
            FactKind::ConfigFile => &mut self.config_files,
            FactKind::OutputDir => &mut self.output_dirs,
            FactKind::ArtifactCandidate => &mut self.artifact_candidates,
            FactKind::CandidateEntryFile => &mut self.candidate_entry_files,
            FactKind::RuntimeMetadata => &mut self.runtime_metadata,
        };
        push_unique(target, value);
    }
}

#[derive(Clone, Copy)]
enum FactKind {
    EvidenceFile,
    ConfigFile,
    OutputDir,
    ArtifactCandidate,
    CandidateEntryFile,
    RuntimeMetadata,
}

fn render_prompt(context: &PromptContext) -> String {
    let mut lines = vec![
        "You are an expert Ato capsule configurator.".to_string(),
        String::new(),
        "Your task is to generate a valid `capsule.toml` for this project.".to_string(),
        "If any requirement is ambiguous, ask concise follow-up questions and wait for the user's answer before writing TOML.".to_string(),
        "Output only the final TOML inside a single ```toml fenced code block after all questions are answered.".to_string(),
        String::new(),
        "## Extracted project facts".to_string(),
        format!("- Project root: `{}`", context.project_dir.display()),
        format!("- Detected project type: {}", context.detected().project_type.as_str()),
        format!("- Suggested package name: `{}`", context.info().name),
    ];

    if !context.framework_matches.is_empty() {
        lines.push(format!(
            "- Framework matches: {}",
            context
                .framework_matches
                .iter()
                .map(|item| format!(
                    "`{}` ({}, confidence {})",
                    item.name, item.category, item.confidence
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    append_fact_list(&mut lines, "Evidence files", &context.evidence_files);
    append_fact_list(&mut lines, "Config files", &context.config_files);
    append_fact_list(&mut lines, "Output directories", &context.output_dirs);
    append_fact_list(
        &mut lines,
        "Artifact candidates",
        &context.artifact_candidates,
    );
    append_fact_list(
        &mut lines,
        "Candidate entry files",
        &context.candidate_entry_files,
    );
    append_fact_list(
        &mut lines,
        "Runtime / bundle metadata",
        &context.runtime_metadata,
    );

    lines.push(String::new());
    lines.push("## Ambiguities to resolve before generating TOML".to_string());
    if context.ambiguities.is_empty() {
        lines.push("- No blocking ambiguities were detected from the filesystem scan. Ask a short follow-up question only if you need information that is not justified by the project facts.".to_string());
    } else {
        for ambiguity in &context.ambiguities {
            lines.push(format!("- {ambiguity}"));
        }
    }

    lines.push(String::new());
    lines.push("## Schema constraints".to_string());
    for constraint in &context.schema_constraints {
        lines.push(format!("- {constraint}"));
    }

    if !context.decision_rules.is_empty() {
        lines.push(String::new());
        lines.push("## Decision rules".to_string());
        for rule in &context.decision_rules {
            lines.push(format!("- {rule}"));
        }
    }

    lines.push(String::new());
    lines.push("## Task".to_string());
    lines.push("- Review the extracted facts.".to_string());
    lines.push("- Ask every required clarifying question before generating TOML.".to_string());
    lines.push("- Once the user answers, produce the final `capsule.toml`.".to_string());
    lines.push(
        "- Output only a single fenced ```toml code block containing the final TOML.".to_string(),
    );

    lines.join("\n")
}

fn append_fact_list(lines: &mut Vec<String>, label: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    lines.push(format!(
        "- {label}: {}",
        values
            .iter()
            .map(|item| format!("`{item}`"))
            .collect::<Vec<_>>()
            .join(", ")
    ));
}

fn add_generic_ambiguities(
    inputs: &PromptInputs,
    context: &PromptContext,
    ambiguities: &mut Vec<String>,
) {
    if matches!(inputs.detected.project_type, ProjectType::Unknown)
        && context.framework_matches.is_empty()
    {
        ambiguities.push(
            "The project type could not be identified confidently. Ask the user what runtime or artifact should be the default target before generating TOML.".to_string(),
        );
    }

    let has_launch_hint = !inputs.info.entrypoint.is_empty()
        || !context.candidate_entry_files.is_empty()
        || !context.artifact_candidates.is_empty();
    if !has_launch_hint {
        ambiguities.push(
            "No reliable entry command or artifact was detected. Ask the user what command or built output should launch the app.".to_string(),
        );
    }

    if context.has_framework("Rust binary")
        && context
            .candidate_entry_files
            .iter()
            .any(|item| item.starts_with("src/bin/"))
    {
        ambiguities.push(
            "Multiple Rust binary entry candidates may exist under `src/bin/`. Ask the user which binary should be the default target.".to_string(),
        );
    }
}

fn collect_artifact_candidates(dir: &Path, context: &mut PromptContext) -> Result<()> {
    for candidate in [
        "dist",
        "build",
        "out",
        ".next",
        ".output",
        "release/bundle",
        "src-tauri/target/release/bundle",
    ] {
        if dir.join(candidate).exists() {
            context.add_artifact_candidate(candidate.to_string());
        }
    }

    for bundle_dir in [
        "dist",
        "release/bundle/macos",
        "src-tauri/target/release/bundle/macos",
    ] {
        let absolute = dir.join(bundle_dir);
        if !absolute.exists() || !absolute.is_dir() {
            continue;
        }
        collect_app_bundles(dir, &absolute, context)?;
    }

    for candidate in [
        "dist/server.js",
        "dist/index.js",
        "build/server.js",
        "build/index.js",
        ".output/server/index.mjs",
        ".output/public",
    ] {
        let absolute = dir.join(candidate);
        if absolute.exists() {
            context.add_artifact_candidate(relative_display(dir, &absolute));
        }
    }

    Ok(())
}

fn collect_app_bundles(root: &Path, dir: &Path, context: &mut PromptContext) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir()
            && path.extension().and_then(|ext| ext.to_str()) == Some("app")
        {
            context.add_artifact_candidate(relative_display(root, &path));
        }
    }
    Ok(())
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn read_optional_text(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn read_optional_json(path: &Path) -> Option<serde_json::Value> {
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn read_optional_toml(path: &Path) -> Option<toml::Value> {
    let content = fs::read_to_string(path).ok()?;
    toml::from_str(&content).ok()
}

pub(super) fn push_unique(values: &mut Vec<String>, value: impl Into<String>) {
    let value = value.into();
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn dedup_preserve_order(values: Vec<String>) -> Vec<String> {
    let mut deduped = Vec::new();
    for value in values {
        push_unique(&mut deduped, value);
    }
    deduped
}

pub(super) fn has_package_dependency(package_json: &serde_json::Value, dependency: &str) -> bool {
    ["dependencies", "devDependencies", "peerDependencies"]
        .iter()
        .any(|key| {
            package_json
                .get(key)
                .and_then(|deps| deps.as_object())
                .map(|deps| deps.contains_key(dependency))
                .unwrap_or(false)
        })
}

pub(super) fn cargo_dependency_present(cargo_toml: &toml::Value, dependency: &str) -> bool {
    ["dependencies", "dev-dependencies"].iter().any(|key| {
        cargo_toml
            .get(key)
            .and_then(|deps| deps.as_table())
            .map(|deps| deps.contains_key(dependency))
            .unwrap_or(false)
    })
}

pub(super) fn pyproject_dependency_present(pyproject: &toml::Value, dependency: &str) -> bool {
    let dependency = dependency.to_ascii_lowercase();

    if pyproject
        .get("project")
        .and_then(|project| project.get("dependencies"))
        .and_then(|deps| deps.as_array())
        .map(|deps| {
            deps.iter().any(|item| {
                item.as_str()
                    .map(|value| value.to_ascii_lowercase().contains(&dependency))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
    {
        return true;
    }

    if pyproject
        .get("project")
        .and_then(|project| project.get("optional-dependencies"))
        .and_then(|deps| deps.as_table())
        .map(|table| {
            table.values().any(|value| {
                value
                    .as_array()
                    .map(|items| {
                        items.iter().any(|item| {
                            item.as_str()
                                .map(|value| value.to_ascii_lowercase().contains(&dependency))
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
    {
        return true;
    }

    pyproject
        .get("tool")
        .and_then(|tool| tool.get("poetry"))
        .and_then(|poetry| poetry.get("dependencies"))
        .and_then(|deps| deps.as_table())
        .map(|table| {
            table
                .keys()
                .any(|key| key.to_ascii_lowercase().contains(&dependency))
        })
        .unwrap_or(false)
}

pub(super) fn text_dependency_present(contents: &str, dependency: &str) -> bool {
    let dependency = dependency.to_ascii_lowercase();
    contents
        .lines()
        .map(|line| line.trim().to_ascii_lowercase())
        .any(|line| line.contains(&dependency))
}

fn node_package_manager_label(package_manager: NodePackageManager) -> &'static str {
    match package_manager {
        NodePackageManager::Bun => "bun",
        NodePackageManager::Deno => "deno",
        NodePackageManager::Npm => "npm",
        NodePackageManager::Pnpm => "pnpm",
        NodePackageManager::Yarn => "yarn",
        NodePackageManager::Unknown => "unknown",
    }
}
