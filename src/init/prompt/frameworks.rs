use anyhow::{Context, Result};
use std::fs;

use super::{
    cargo_dependency_present, has_package_dependency, push_unique, pyproject_dependency_present,
    text_dependency_present, FrameworkMatch, PromptContext, PromptInputs,
};

pub(super) struct PromptRule {
    pub framework: FrameworkMatch,
    pub detect: fn(&PromptInputs) -> Result<bool>,
    pub add_facts: fn(&PromptInputs, &mut PromptContext) -> Result<()>,
    pub add_ambiguities: fn(&PromptInputs, &PromptContext, &mut Vec<String>) -> Result<()>,
    pub add_schema_constraints: fn(&PromptContext, &mut Vec<String>),
    pub add_decision_rules: fn(&PromptContext, &mut Vec<String>),
}

pub(super) fn match_rules(inputs: &PromptInputs) -> Result<Vec<&'static PromptRule>> {
    let mut matched = Vec::new();
    for rule in FRAMEWORK_RULES {
        if (rule.detect)(inputs)? {
            matched.push(rule);
        }
    }
    Ok(matched)
}

pub(super) fn detect_node_frameworks(inputs: &PromptInputs) -> Result<Vec<FrameworkMatch>> {
    let mut frameworks = Vec::new();
    let Some(package_json) = inputs.package_json.as_ref() else {
        return Ok(frameworks);
    };

    for framework in [
        direct_match(package_json, "next", "Next.js", "web-ssr", 95),
        direct_match(package_json, "astro", "Astro", "web-static-ssr", 95),
        direct_match(package_json, "nuxt", "Nuxt", "web-ssr", 95),
        direct_match(package_json, "@sveltejs/kit", "SvelteKit", "web-ssr", 95),
        direct_match(package_json, "react", "React", "web-spa", 80),
        direct_match(package_json, "vue", "Vue", "web-spa", 80),
        direct_match(package_json, "vite", "Vite", "web-build", 90),
        direct_match(package_json, "express", "Express", "api-server", 90),
        direct_match(package_json, "fastify", "Fastify", "api-server", 90),
        direct_match(package_json, "@nestjs/core", "NestJS", "api-server", 95),
        direct_match(package_json, "hono", "Hono", "api-server", 90),
        direct_match(package_json, "electron", "Electron", "native-desktop", 95),
    ]
    .into_iter()
    .flatten()
    {
        frameworks.push(framework);
    }

    if has_package_dependency(package_json, "react") && has_package_dependency(package_json, "vite")
    {
        frameworks.push(FrameworkMatch {
            name: "React + Vite",
            category: "web-spa",
            confidence: 95,
        });
    }
    if has_package_dependency(package_json, "vue") && has_package_dependency(package_json, "vite") {
        frameworks.push(FrameworkMatch {
            name: "Vue + Vite",
            category: "web-spa",
            confidence: 95,
        });
    }

    if let Some(main) = package_json.get("main").and_then(|value| value.as_str()) {
        if !main.trim().is_empty() && !frameworks.iter().any(|item| item.name == "Electron") {
            let looks_like_electron = ["electron", "electron/main", "main.js", "background.js"]
                .iter()
                .any(|needle| main.contains(needle));
            if looks_like_electron {
                frameworks.push(FrameworkMatch {
                    name: "Electron",
                    category: "native-desktop",
                    confidence: 75,
                });
            }
        }
    }

    Ok(frameworks)
}

pub(super) fn detect_python_frameworks(inputs: &PromptInputs) -> Result<Vec<FrameworkMatch>> {
    let mut frameworks = Vec::new();
    let has_dep = |dependency: &str| {
        inputs
            .requirements_txt
            .as_deref()
            .map(|contents| text_dependency_present(contents, dependency))
            .unwrap_or(false)
            || inputs
                .pyproject_toml
                .as_ref()
                .map(|pyproject| pyproject_dependency_present(pyproject, dependency))
                .unwrap_or(false)
    };

    if has_dep("fastapi") {
        frameworks.push(FrameworkMatch {
            name: "FastAPI",
            category: "python-web",
            confidence: 95,
        });
    }
    if has_dep("flask") {
        frameworks.push(FrameworkMatch {
            name: "Flask",
            category: "python-web",
            confidence: 90,
        });
    }
    if has_dep("django") {
        frameworks.push(FrameworkMatch {
            name: "Django",
            category: "python-web",
            confidence: 95,
        });
    }
    if has_dep("streamlit") {
        frameworks.push(FrameworkMatch {
            name: "Streamlit",
            category: "python-app",
            confidence: 95,
        });
    }

    Ok(frameworks)
}

pub(super) fn detect_rust_frameworks(inputs: &PromptInputs) -> Result<Vec<FrameworkMatch>> {
    let mut frameworks = Vec::new();
    if let Some(cargo_toml) = inputs.cargo_toml.as_ref() {
        if cargo_dependency_present(cargo_toml, "axum") {
            frameworks.push(FrameworkMatch {
                name: "Axum",
                category: "rust-web",
                confidence: 95,
            });
        }
        if cargo_dependency_present(cargo_toml, "actix-web") {
            frameworks.push(FrameworkMatch {
                name: "Actix Web",
                category: "rust-web",
                confidence: 95,
            });
        }
        if cargo_dependency_present(cargo_toml, "rocket") {
            frameworks.push(FrameworkMatch {
                name: "Rocket",
                category: "rust-web",
                confidence: 95,
            });
        }
        let has_web_framework = frameworks.iter().any(|item| item.category == "rust-web");
        if !has_web_framework
            && matches!(
                inputs.detected.project_type,
                crate::init::detect::ProjectType::Rust
            )
        {
            frameworks.push(FrameworkMatch {
                name: "Rust binary",
                category: "rust-app",
                confidence: 80,
            });
        }
    }
    Ok(frameworks)
}

pub(super) fn detect_go_frameworks(inputs: &PromptInputs) -> Result<Vec<FrameworkMatch>> {
    let mut frameworks = Vec::new();
    let Some(go_mod) = inputs.go_mod.as_deref() else {
        return Ok(frameworks);
    };

    for (needle, name) in [
        ("github.com/gin-gonic/gin", "Gin"),
        ("github.com/gofiber/fiber", "Fiber"),
        ("github.com/labstack/echo", "Echo"),
    ] {
        if text_dependency_present(go_mod, needle) {
            frameworks.push(FrameworkMatch {
                name,
                category: "go-web",
                confidence: 95,
            });
        }
    }

    Ok(frameworks)
}

pub(super) fn detect_native_frameworks(inputs: &PromptInputs) -> Result<Vec<FrameworkMatch>> {
    let mut frameworks = Vec::new();
    let has_tauri = inputs.dir.join("src-tauri").is_dir()
        || inputs.dir.join("src-tauri/Cargo.toml").is_file()
        || inputs.dir.join("tauri.conf.json").is_file()
        || inputs.dir.join("src-tauri/tauri.conf.json").is_file();
    if has_tauri {
        frameworks.push(FrameworkMatch {
            name: "Tauri",
            category: "native-desktop",
            confidence: 95,
        });
    }

    let has_electron = inputs
        .package_json
        .as_ref()
        .map(|package_json| {
            has_package_dependency(package_json, "electron")
                || has_package_dependency(package_json, "electron-builder")
                || package_json
                    .get("devDependencies")
                    .and_then(|deps| deps.as_object())
                    .map(|deps| deps.keys().any(|key| key.starts_with("@electron-forge/")))
                    .unwrap_or(false)
                || package_json
                    .get("main")
                    .and_then(|value| value.as_str())
                    .is_some()
        })
        .unwrap_or(false)
        || inputs.dir.join("electron-builder.json").is_file()
        || inputs.dir.join("electron-builder.yml").is_file()
        || inputs.dir.join("electron-builder.yaml").is_file()
        || inputs.dir.join("forge.config.js").is_file()
        || inputs.dir.join("forge.config.ts").is_file();
    if has_electron {
        frameworks.push(FrameworkMatch {
            name: "Electron",
            category: "native-desktop",
            confidence: 90,
        });
    }

    Ok(frameworks)
}

fn direct_match(
    package_json: &serde_json::Value,
    dependency: &str,
    name: &'static str,
    category: &'static str,
    confidence: u8,
) -> Option<FrameworkMatch> {
    has_package_dependency(package_json, dependency).then_some(FrameworkMatch {
        name,
        category,
        confidence,
    })
}

fn framework_detected(frameworks: &[FrameworkMatch], target: &str) -> bool {
    frameworks.iter().any(|item| item.name == target)
}

fn detect_next(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(
        &detect_node_frameworks(inputs)?,
        "Next.js",
    ))
}

fn detect_astro(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(
        &detect_node_frameworks(inputs)?,
        "Astro",
    ))
}

fn detect_nuxt(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(&detect_node_frameworks(inputs)?, "Nuxt"))
}

fn detect_sveltekit(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(
        &detect_node_frameworks(inputs)?,
        "SvelteKit",
    ))
}

fn detect_react(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(
        &detect_node_frameworks(inputs)?,
        "React",
    ))
}

fn detect_vue(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(&detect_node_frameworks(inputs)?, "Vue"))
}

fn detect_vite(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(&detect_node_frameworks(inputs)?, "Vite"))
}

fn detect_react_vite(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(
        &detect_node_frameworks(inputs)?,
        "React + Vite",
    ))
}

fn detect_vue_vite(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(
        &detect_node_frameworks(inputs)?,
        "Vue + Vite",
    ))
}

fn detect_express(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(
        &detect_node_frameworks(inputs)?,
        "Express",
    ))
}

fn detect_fastify(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(
        &detect_node_frameworks(inputs)?,
        "Fastify",
    ))
}

fn detect_nest(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(
        &detect_node_frameworks(inputs)?,
        "NestJS",
    ))
}

fn detect_hono(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(&detect_node_frameworks(inputs)?, "Hono"))
}

fn detect_tauri(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(
        &detect_native_frameworks(inputs)?,
        "Tauri",
    ))
}

fn detect_electron(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(
        &detect_native_frameworks(inputs)?,
        "Electron",
    ))
}

fn detect_fastapi(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(
        &detect_python_frameworks(inputs)?,
        "FastAPI",
    ))
}

fn detect_django(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(
        &detect_python_frameworks(inputs)?,
        "Django",
    ))
}

fn detect_streamlit(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(
        &detect_python_frameworks(inputs)?,
        "Streamlit",
    ))
}

fn detect_axum(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(&detect_rust_frameworks(inputs)?, "Axum"))
}

fn detect_actix(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(
        &detect_rust_frameworks(inputs)?,
        "Actix Web",
    ))
}

fn detect_rocket(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(
        &detect_rust_frameworks(inputs)?,
        "Rocket",
    ))
}

fn detect_rust_binary(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(
        &detect_rust_frameworks(inputs)?,
        "Rust binary",
    ))
}

fn detect_gin(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(&detect_go_frameworks(inputs)?, "Gin"))
}

fn detect_fiber(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(&detect_go_frameworks(inputs)?, "Fiber"))
}

fn detect_echo(inputs: &PromptInputs) -> Result<bool> {
    Ok(framework_detected(&detect_go_frameworks(inputs)?, "Echo"))
}

fn noop_add_ambiguities(
    _inputs: &PromptInputs,
    _context: &PromptContext,
    _ambiguities: &mut Vec<String>,
) -> Result<()> {
    Ok(())
}

fn noop_add_schema_constraints(_context: &PromptContext, _constraints: &mut Vec<String>) {}

fn noop_add_decision_rules(_context: &PromptContext, _rules: &mut Vec<String>) {}

fn add_web_output_facts(inputs: &PromptInputs, context: &mut PromptContext) -> Result<()> {
    for path in ["dist", "build", "out", ".next", ".output", "public"] {
        if inputs.dir.join(path).exists() {
            context.add_output_dir(path.to_string());
        }
    }
    Ok(())
}

fn add_server_entry_facts(inputs: &PromptInputs, context: &mut PromptContext) -> Result<()> {
    add_web_output_facts(inputs, context)?;
    for path in [
        "server.js",
        "app.js",
        "index.js",
        "dist/server.js",
        "dist/index.js",
        "src/index.ts",
        "src/main.ts",
        "src/index.js",
        "src/main.js",
        "main.ts",
        "main.js",
    ] {
        if inputs.dir.join(path).exists() {
            context.add_candidate_entry_file(path.to_string());
        }
    }
    Ok(())
}

fn add_tauri_facts(inputs: &PromptInputs, context: &mut PromptContext) -> Result<()> {
    for path in [
        "src-tauri/Cargo.toml",
        "tauri.conf.json",
        "src-tauri/tauri.conf.json",
    ] {
        if inputs.dir.join(path).is_file() {
            context.add_evidence_file(path.to_string());
        }
    }
    if inputs.dir.join("src-tauri").is_dir() {
        context.add_output_dir("src-tauri".to_string());
    }
    for path in [
        "src-tauri/target",
        "src-tauri/target/release/bundle",
        "release/bundle",
        "release/bundle/macos",
    ] {
        if inputs.dir.join(path).exists() {
            context.add_output_dir(path.to_string());
        }
    }
    context.add_runtime_metadata(
        "Native desktop bundle metadata detected via Tauri config".to_string(),
    );
    Ok(())
}

fn add_electron_facts(inputs: &PromptInputs, context: &mut PromptContext) -> Result<()> {
    for path in [
        "electron-builder.json",
        "electron-builder.yml",
        "electron-builder.yaml",
        "forge.config.js",
        "forge.config.ts",
    ] {
        if inputs.dir.join(path).is_file() {
            context.add_evidence_file(path.to_string());
            context.add_config_file(path.to_string());
        }
    }
    context.add_runtime_metadata(
        "Electron packaging metadata detected; prefer packaged app artifacts over `electron .` dev mode"
            .to_string(),
    );
    Ok(())
}

fn add_python_entry_facts(inputs: &PromptInputs, context: &mut PromptContext) -> Result<()> {
    for path in ["main.py", "app.py", "server.py", "manage.py"] {
        if inputs.dir.join(path).is_file() {
            context.add_candidate_entry_file(path.to_string());
        }
    }
    Ok(())
}

fn add_rust_entry_facts(inputs: &PromptInputs, context: &mut PromptContext) -> Result<()> {
    if inputs.dir.join("Cargo.toml").is_file() {
        context.add_evidence_file("Cargo.toml".to_string());
    }
    if inputs.dir.join("src/main.rs").is_file() {
        context.add_candidate_entry_file("src/main.rs".to_string());
    }
    if inputs.dir.join("src/bin").is_dir() {
        context.add_candidate_entry_file("src/bin".to_string());
    }
    if inputs.dir.join("target").is_dir() {
        context.add_output_dir("target".to_string());
    }
    Ok(())
}

fn detect_next_ambiguity(
    inputs: &PromptInputs,
    _context: &PromptContext,
    ambiguities: &mut Vec<String>,
) -> Result<()> {
    if !next_static_export_detected(inputs)? {
        ambiguities.push(
            "This looks like a Next.js project, but it is unclear whether the intended deployment is a static export (`out/`) or a dynamic server (`next start` / SSR). Ask the user which mode they want before generating TOML.".to_string(),
        );
    }
    Ok(())
}

fn detect_astro_ambiguity(
    _inputs: &PromptInputs,
    _context: &PromptContext,
    ambiguities: &mut Vec<String>,
) -> Result<()> {
    ambiguities.push(
        "This looks like an Astro project. Ask whether the app should be deployed as a static site or with an SSR adapter before generating TOML.".to_string(),
    );
    Ok(())
}

fn detect_nuxt_ambiguity(
    _inputs: &PromptInputs,
    _context: &PromptContext,
    ambiguities: &mut Vec<String>,
) -> Result<()> {
    ambiguities.push(
        "This looks like a Nuxt project. Ask whether the intended output is a static generate build or a server deployment before generating TOML.".to_string(),
    );
    Ok(())
}

fn detect_sveltekit_ambiguity(
    _inputs: &PromptInputs,
    _context: &PromptContext,
    ambiguities: &mut Vec<String>,
) -> Result<()> {
    ambiguities.push(
        "This looks like a SvelteKit project. Ask whether the app uses a static adapter or a server adapter before generating TOML.".to_string(),
    );
    Ok(())
}

fn detect_react_vite_ambiguity(
    _inputs: &PromptInputs,
    _context: &PromptContext,
    ambiguities: &mut Vec<String>,
) -> Result<()> {
    ambiguities.push(
        "This looks like a React + Vite app. Ask whether it should be served as a purely static site or whether a separate Node server is part of the deployment.".to_string(),
    );
    Ok(())
}

fn detect_vue_vite_ambiguity(
    _inputs: &PromptInputs,
    _context: &PromptContext,
    ambiguities: &mut Vec<String>,
) -> Result<()> {
    ambiguities.push(
        "This looks like a Vue + Vite app. Ask whether it should be deployed as a static site or paired with a separate server runtime.".to_string(),
    );
    Ok(())
}

fn detect_server_entry_ambiguity(
    _inputs: &PromptInputs,
    context: &PromptContext,
    ambiguities: &mut Vec<String>,
) -> Result<()> {
    let has_dist_entry = context
        .candidate_entry_files
        .iter()
        .any(|item| item.starts_with("dist/"));
    if !has_dist_entry {
        ambiguities.push(
            "Ask whether this server should run directly from source (for example TypeScript tooling) or from a built `dist/` artifact, and confirm which entry file is authoritative.".to_string(),
        );
    }
    Ok(())
}

fn detect_fastapi_ambiguity(
    _inputs: &PromptInputs,
    context: &PromptContext,
    ambiguities: &mut Vec<String>,
) -> Result<()> {
    let has_main_app = context
        .candidate_entry_files
        .iter()
        .any(|item| item == "main.py" || item == "app.py");
    if !has_main_app {
        ambiguities.push(
            "This looks like a FastAPI project, but the `uvicorn module:app` import path is unclear. Ask the user which module and app object should be used.".to_string(),
        );
    } else {
        ambiguities.push(
            "Confirm the FastAPI `uvicorn module:app` import path and app object name before generating TOML.".to_string(),
        );
    }
    Ok(())
}

fn detect_django_ambiguity(
    _inputs: &PromptInputs,
    _context: &PromptContext,
    ambiguities: &mut Vec<String>,
) -> Result<()> {
    ambiguities.push(
        "This looks like a Django project. Ask whether the user wants a development `manage.py runserver` flow or a production ASGI/WSGI entry before generating TOML.".to_string(),
    );
    Ok(())
}

fn detect_streamlit_ambiguity(
    _inputs: &PromptInputs,
    context: &PromptContext,
    ambiguities: &mut Vec<String>,
) -> Result<()> {
    let has_script = context
        .candidate_entry_files
        .iter()
        .any(|item| item.ends_with(".py"));
    if !has_script {
        ambiguities.push(
            "This looks like a Streamlit app, but the app script is unclear. Ask the user which `.py` entry file should be launched.".to_string(),
        );
    }
    Ok(())
}

fn detect_tauri_ambiguity(
    _inputs: &PromptInputs,
    context: &PromptContext,
    ambiguities: &mut Vec<String>,
) -> Result<()> {
    ambiguities.push(
        "This looks like a Tauri project. Ask which final native bundle artifact should be treated as the primary output, and confirm that the frontend is embedded in the desktop app rather than shipped as a standalone web target.".to_string(),
    );
    if !context.has_artifact_suffix(".app") {
        ambiguities.push(
            "No built `.app` bundle was detected yet. Ask the user which bundle output directory will contain the native artifact after build.".to_string(),
        );
    }
    Ok(())
}

fn detect_electron_ambiguity(
    _inputs: &PromptInputs,
    _context: &PromptContext,
    ambiguities: &mut Vec<String>,
) -> Result<()> {
    ambiguities.push(
        "This looks like an Electron project. Ask whether the target should be a packaged desktop artifact rather than the development command `electron .`, and confirm which bundle output to use.".to_string(),
    );
    Ok(())
}

fn add_static_schema_constraint(_context: &PromptContext, constraints: &mut Vec<String>) {
    push_unique(
        constraints,
        "For static web output, use `[targets.static]`, `runtime = \"web\"`, `driver = \"static\"`, and point `entrypoint` at the built output directory.".to_string(),
    );
}

fn add_source_server_constraint(_context: &PromptContext, constraints: &mut Vec<String>) {
    push_unique(
        constraints,
        "For server-style apps, prefer a `cli` target with `runtime = \"source\"`, and use the confirmed run command as `entrypoint` + `cmd` values.".to_string(),
    );
}

fn add_native_constraint(_context: &PromptContext, constraints: &mut Vec<String>) {
    push_unique(
        constraints,
        "Do not misclassify a native desktop app as a standalone static web target when the primary artifact is a bundled desktop app.".to_string(),
    );
}

fn add_next_decision_rules(_context: &PromptContext, rules: &mut Vec<String>) {
    push_unique(
        rules,
        "If the user confirms static export, generate a `static` web target rooted at `out` unless the project facts show a different export directory.".to_string(),
    );
    push_unique(
        rules,
        "If the user confirms SSR / dynamic server mode, generate a `cli` source target using the confirmed release command (for example `npm start`).".to_string(),
    );
}

fn add_astro_decision_rules(_context: &PromptContext, rules: &mut Vec<String>) {
    push_unique(
        rules,
        "If the user confirms Astro static output, prefer a `static` web target with `entrypoint = \"dist\"` when `dist/` is the build output.".to_string(),
    );
}

fn add_nuxt_decision_rules(context: &PromptContext, rules: &mut Vec<String>) {
    push_unique(
        rules,
        if context.has_output_dir(".output") {
            "If the user confirms Nuxt static generation, prefer a `static` web target rooted at `.output/public` when that directory exists; otherwise ask for the generated directory.".to_string()
        } else {
            "If the user confirms Nuxt static generation, ask which generated directory should back the static web target before producing TOML.".to_string()
        },
    );
    if !context.release_command().is_empty() {
        push_unique(
            rules,
            format!(
                "If the user confirms a server deployment, prefer the confirmed server command (current hint: `{}`).",
                context.release_command()
            ),
        );
    }
}

fn add_sveltekit_decision_rules(_context: &PromptContext, rules: &mut Vec<String>) {
    push_unique(
        rules,
        "If the user confirms a static adapter, prefer a `static` web target; if they confirm a server adapter, use a `cli` source target with the confirmed server command.".to_string(),
    );
}

fn add_vite_static_decision_rules(context: &PromptContext, rules: &mut Vec<String>) {
    push_unique(
        rules,
        if context.has_output_dir("dist") {
            "If the app is a pure Vite static build, prefer a `static` web target with `entrypoint = \"dist\"` when `dist/` is present.".to_string()
        } else {
            "If the app is a pure Vite static build, ask which generated directory should become the static web target before producing TOML.".to_string()
        },
    );
}

fn add_server_decision_rules(context: &PromptContext, rules: &mut Vec<String>) {
    if !context.release_command().is_empty() {
        push_unique(
            rules,
            format!(
                "Prefer a `cli` source target using the confirmed server command (current hint: `{}`).",
                context.release_command()
            ),
        );
    }
}

fn add_fastapi_decision_rules(_context: &PromptContext, rules: &mut Vec<String>) {
    push_unique(
        rules,
        "For FastAPI, prefer a `cli` source target with a confirmed `uvicorn module:app --host 0.0.0.0 --port 8000` style command.".to_string(),
    );
}

fn add_tauri_decision_rules(_context: &PromptContext, rules: &mut Vec<String>) {
    push_unique(
        rules,
        "If the primary deliverable is a Tauri bundle, do not emit a standalone static web target for the frontend alone unless the user explicitly asks for that.".to_string(),
    );
    push_unique(
        rules,
        "Ask the user to confirm the native bundle output directory or artifact path before generating TOML for a desktop-targeted flow.".to_string(),
    );
}

fn add_electron_decision_rules(_context: &PromptContext, rules: &mut Vec<String>) {
    push_unique(
        rules,
        "If the primary deliverable is a packaged Electron app, prefer the packaged desktop artifact and do not default to the development command `electron .`.".to_string(),
    );
}

fn add_rust_binary_decision_rules(_context: &PromptContext, rules: &mut Vec<String>) {
    push_unique(
        rules,
        "For a plain Rust binary, prefer a `cli` source target whose runtime command points at the built binary artifact rather than `cargo run` for release packaging.".to_string(),
    );
}

fn next_static_export_detected(inputs: &PromptInputs) -> Result<bool> {
    if inputs.dir.join("out").exists() {
        return Ok(true);
    }

    for config_name in [
        "next.config.js",
        "next.config.mjs",
        "next.config.cjs",
        "next.config.ts",
    ] {
        let path = inputs.dir.join(config_name);
        if !path.exists() {
            continue;
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let normalized = content.replace([' ', '\n', '\r', '\t'], "");
        if normalized.contains("output:\"export\"") || normalized.contains("output:'export'") {
            return Ok(true);
        }
    }

    Ok(false)
}

const FRAMEWORK_RULES: &[PromptRule] = &[
    PromptRule {
        framework: FrameworkMatch {
            name: "Next.js",
            category: "web-ssr",
            confidence: 95,
        },
        detect: detect_next,
        add_facts: add_web_output_facts,
        add_ambiguities: detect_next_ambiguity,
        add_schema_constraints: add_static_schema_constraint,
        add_decision_rules: add_next_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Astro",
            category: "web-static-ssr",
            confidence: 95,
        },
        detect: detect_astro,
        add_facts: add_web_output_facts,
        add_ambiguities: detect_astro_ambiguity,
        add_schema_constraints: add_static_schema_constraint,
        add_decision_rules: add_astro_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Nuxt",
            category: "web-ssr",
            confidence: 95,
        },
        detect: detect_nuxt,
        add_facts: add_web_output_facts,
        add_ambiguities: detect_nuxt_ambiguity,
        add_schema_constraints: add_static_schema_constraint,
        add_decision_rules: add_nuxt_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "SvelteKit",
            category: "web-ssr",
            confidence: 95,
        },
        detect: detect_sveltekit,
        add_facts: add_web_output_facts,
        add_ambiguities: detect_sveltekit_ambiguity,
        add_schema_constraints: add_static_schema_constraint,
        add_decision_rules: add_sveltekit_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "React",
            category: "web-spa",
            confidence: 80,
        },
        detect: detect_react,
        add_facts: add_web_output_facts,
        add_ambiguities: noop_add_ambiguities,
        add_schema_constraints: noop_add_schema_constraints,
        add_decision_rules: noop_add_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Vue",
            category: "web-spa",
            confidence: 80,
        },
        detect: detect_vue,
        add_facts: add_web_output_facts,
        add_ambiguities: noop_add_ambiguities,
        add_schema_constraints: noop_add_schema_constraints,
        add_decision_rules: noop_add_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Vite",
            category: "web-build",
            confidence: 90,
        },
        detect: detect_vite,
        add_facts: add_web_output_facts,
        add_ambiguities: noop_add_ambiguities,
        add_schema_constraints: noop_add_schema_constraints,
        add_decision_rules: noop_add_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "React + Vite",
            category: "web-spa",
            confidence: 95,
        },
        detect: detect_react_vite,
        add_facts: add_web_output_facts,
        add_ambiguities: detect_react_vite_ambiguity,
        add_schema_constraints: add_static_schema_constraint,
        add_decision_rules: add_vite_static_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Vue + Vite",
            category: "web-spa",
            confidence: 95,
        },
        detect: detect_vue_vite,
        add_facts: add_web_output_facts,
        add_ambiguities: detect_vue_vite_ambiguity,
        add_schema_constraints: add_static_schema_constraint,
        add_decision_rules: add_vite_static_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Express",
            category: "api-server",
            confidence: 90,
        },
        detect: detect_express,
        add_facts: add_server_entry_facts,
        add_ambiguities: detect_server_entry_ambiguity,
        add_schema_constraints: add_source_server_constraint,
        add_decision_rules: add_server_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Fastify",
            category: "api-server",
            confidence: 90,
        },
        detect: detect_fastify,
        add_facts: add_server_entry_facts,
        add_ambiguities: detect_server_entry_ambiguity,
        add_schema_constraints: add_source_server_constraint,
        add_decision_rules: add_server_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "NestJS",
            category: "api-server",
            confidence: 95,
        },
        detect: detect_nest,
        add_facts: add_server_entry_facts,
        add_ambiguities: detect_server_entry_ambiguity,
        add_schema_constraints: add_source_server_constraint,
        add_decision_rules: add_server_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Hono",
            category: "api-server",
            confidence: 90,
        },
        detect: detect_hono,
        add_facts: add_server_entry_facts,
        add_ambiguities: detect_server_entry_ambiguity,
        add_schema_constraints: add_source_server_constraint,
        add_decision_rules: add_server_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Tauri",
            category: "native-desktop",
            confidence: 95,
        },
        detect: detect_tauri,
        add_facts: add_tauri_facts,
        add_ambiguities: detect_tauri_ambiguity,
        add_schema_constraints: add_native_constraint,
        add_decision_rules: add_tauri_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Electron",
            category: "native-desktop",
            confidence: 90,
        },
        detect: detect_electron,
        add_facts: add_electron_facts,
        add_ambiguities: detect_electron_ambiguity,
        add_schema_constraints: add_native_constraint,
        add_decision_rules: add_electron_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "FastAPI",
            category: "python-web",
            confidence: 95,
        },
        detect: detect_fastapi,
        add_facts: add_python_entry_facts,
        add_ambiguities: detect_fastapi_ambiguity,
        add_schema_constraints: add_source_server_constraint,
        add_decision_rules: add_fastapi_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Django",
            category: "python-web",
            confidence: 95,
        },
        detect: detect_django,
        add_facts: add_python_entry_facts,
        add_ambiguities: detect_django_ambiguity,
        add_schema_constraints: add_source_server_constraint,
        add_decision_rules: add_server_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Streamlit",
            category: "python-app",
            confidence: 95,
        },
        detect: detect_streamlit,
        add_facts: add_python_entry_facts,
        add_ambiguities: detect_streamlit_ambiguity,
        add_schema_constraints: add_source_server_constraint,
        add_decision_rules: add_server_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Axum",
            category: "rust-web",
            confidence: 95,
        },
        detect: detect_axum,
        add_facts: add_rust_entry_facts,
        add_ambiguities: noop_add_ambiguities,
        add_schema_constraints: add_source_server_constraint,
        add_decision_rules: add_server_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Actix Web",
            category: "rust-web",
            confidence: 95,
        },
        detect: detect_actix,
        add_facts: add_rust_entry_facts,
        add_ambiguities: noop_add_ambiguities,
        add_schema_constraints: add_source_server_constraint,
        add_decision_rules: add_server_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Rocket",
            category: "rust-web",
            confidence: 95,
        },
        detect: detect_rocket,
        add_facts: add_rust_entry_facts,
        add_ambiguities: noop_add_ambiguities,
        add_schema_constraints: add_source_server_constraint,
        add_decision_rules: add_server_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Rust binary",
            category: "rust-app",
            confidence: 80,
        },
        detect: detect_rust_binary,
        add_facts: add_rust_entry_facts,
        add_ambiguities: noop_add_ambiguities,
        add_schema_constraints: add_source_server_constraint,
        add_decision_rules: add_rust_binary_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Gin",
            category: "go-web",
            confidence: 95,
        },
        detect: detect_gin,
        add_facts: add_server_entry_facts,
        add_ambiguities: detect_server_entry_ambiguity,
        add_schema_constraints: add_source_server_constraint,
        add_decision_rules: add_server_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Fiber",
            category: "go-web",
            confidence: 95,
        },
        detect: detect_fiber,
        add_facts: add_server_entry_facts,
        add_ambiguities: detect_server_entry_ambiguity,
        add_schema_constraints: add_source_server_constraint,
        add_decision_rules: add_server_decision_rules,
    },
    PromptRule {
        framework: FrameworkMatch {
            name: "Echo",
            category: "go-web",
            confidence: 95,
        },
        detect: detect_echo,
        add_facts: add_server_entry_facts,
        add_ambiguities: detect_server_entry_ambiguity,
        add_schema_constraints: add_source_server_constraint,
        add_decision_rules: add_server_decision_rules,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn detects_node_frameworks_and_combo_rules() {
        let tmp = TempDir::new().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{
  "dependencies": {
    "react": "^19.0.0",
    "vite": "^5.0.0",
    "express": "^4.0.0"
  }
}"#,
        )
        .unwrap();
        let detected = crate::init::detect::detect_project(tmp.path()).unwrap();
        let info = crate::init::recipe::project_info_from_detection(&detected).unwrap();
        let inputs = PromptInputs::load(tmp.path(), &detected, &info).unwrap();

        let frameworks = detect_node_frameworks(&inputs).unwrap();
        assert!(framework_detected(&frameworks, "React"));
        assert!(framework_detected(&frameworks, "Vite"));
        assert!(framework_detected(&frameworks, "React + Vite"));
        assert!(framework_detected(&frameworks, "Express"));
    }

    #[test]
    fn detects_native_frameworks() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("src-tauri")).unwrap();
        fs::write(
            tmp.path().join("src-tauri/Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();

        let detected = crate::init::detect::detect_project(tmp.path()).unwrap();
        let info = crate::init::recipe::project_info_from_detection(&detected).unwrap();
        let inputs = PromptInputs::load(tmp.path(), &detected, &info).unwrap();
        let frameworks = detect_native_frameworks(&inputs).unwrap();
        assert!(framework_detected(&frameworks, "Tauri"));
    }
}
