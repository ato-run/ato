//! App Presets — the small set of shapes Ato knows how to turn into an App.
//!
//! ## Why presets, and not a configuration file
//!
//! The alternative is to let people describe their runtime: a build command, an
//! output directory, a package manager, a port, a mount. That is a deployment
//! product, and it asks a person who wanted to keep an HTML file to first learn
//! what a build command is. It also pushes Formation back toward the inference
//! state machine the legacy Builder became — measured, on staging, building a
//! 6 GB Python VM image for one `.html` file, because the model it had could
//! only express "something that runs".
//!
//! So the contract inverts. Ato names a few App shapes; software either fits
//! one or is told plainly that it does not. The rules stay small enough to
//! print, and Ato needs no settings screen at all.
//!
//! ## A preset is authoring provenance, not runtime truth
//!
//! ```text
//! single-html/v1
//!       | compile once
//!       v
//! ProgramIntent  ->  EffectiveBuildPlan  ->  ComputeSchema
//! ```
//!
//! The ComputeSchema stores the RESOLVED semantics, never the preset name to be
//! re-interpreted later. That is what makes versioning honest: if
//! `node-static/v1` means `npm ci → npm run build → dist/`, it means that
//! forever, and every artifact already formed under it keeps meaning what it
//! meant. A different rule is a different preset — `v2` — and never a quiet
//! redefinition of `v1`.
//!
//! ## What is deliberately NOT here
//!
//! Framework detection. There is no React / Vue / Svelte / Astro branch, and
//! there will not be one: guessing a framework is how the inference machine
//! grows back. `node-static/v1` asks for `package.json`, a lockfile, and a
//! `build` script that writes `dist/` — and when a project does not do that,
//! it says so in one sentence a person can act on, rather than trying six more
//! heuristics.

use serde::{Deserialize, Serialize};

use crate::detect::DetectorEvidence;

/// The preset identifiers, as they appear in provenance and in authoring.
pub const SINGLE_HTML_V1: &str = "single-html/v1";
pub const STATIC_FILES_V1: &str = "static-files/v1";
pub const NODE_STATIC_V1: &str = "node-static/v1";

/// The one entry document every Static preset produces.
pub const CANONICAL_ENTRY: &str = "index.html";

/// The output directory `node-static/v1` requires. Fixed, on purpose — see the
/// module note about settings screens.
pub const NODE_STATIC_OUTPUT_ROOT: &str = "dist";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppPreset {
    /// Exactly one `.html`. No build, no dependencies, no server.
    SingleHtml,
    /// A tree with a root `index.html`. No build.
    StaticFiles,
    /// `npm ci` → `npm run build` → `dist/`. Node is a build tool, not a runtime.
    NodeStatic,
}

impl AppPreset {
    pub fn id(self) -> &'static str {
        match self {
            Self::SingleHtml => SINGLE_HTML_V1,
            Self::StaticFiles => STATIC_FILES_V1,
            Self::NodeStatic => NODE_STATIC_V1,
        }
    }

    /// What to call this in front of a person.
    ///
    /// "Node" does not appear: it is the tool that BUILDS the App, not the
    /// thing the App runs on, and naming it here would re-teach the boundary
    /// B1 spent its whole acceptance establishing.
    pub fn label(self) -> &'static str {
        match self {
            Self::SingleHtml => "Single HTML",
            Self::StaticFiles => "Static website",
            Self::NodeStatic => "Built web app",
        }
    }

    /// Does forming this preset resolve dependencies from the network?
    ///
    /// This is the security-matrix question, and it belongs to the preset
    /// rather than to a scan of the source: `node-static/v1` installs from a
    /// registry and is therefore trusted/allowlist-only, while the two
    /// build-free presets can take public untrusted uploads.
    pub fn resolves_dependencies(self) -> bool {
        matches!(self, Self::NodeStatic)
    }

    pub fn parse(id: &str) -> Option<Self> {
        match id {
            SINGLE_HTML_V1 => Some(Self::SingleHtml),
            STATIC_FILES_V1 => Some(Self::StaticFiles),
            NODE_STATIC_V1 => Some(Self::NodeStatic),
            _ => None,
        }
    }
}

/// Why a source does not fit any preset, in words a person can act on.
///
/// Never "no lane matched": that names our dispatch, not their problem. Each
/// message says what Ato looked for, so the next move is obvious.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetMismatch {
    pub code: &'static str,
    pub message: String,
}

impl PresetMismatch {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

fn has(evidence: &DetectorEvidence, name: &str) -> bool {
    evidence.present_files.iter().any(|entry| entry == name)
}

/// Files that are part of the app, as opposed to part of the repository.
///
/// A single-HTML upload is one file; a folder someone dragged in may also carry
/// a README or a `.gitignore`, and refusing it for that would be pedantry about
/// something the person did not put there on purpose.
fn is_incidental(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with('.')
        || lower.ends_with(".md")
        || lower == "license"
        || lower == "license.txt"
        || lower == "capsule.toml"
}

/// Pick the preset a source fits, or say why none does.
///
/// Order matters and is stated: the most constrained shape is tried first, so a
/// lone `.html` is `single-html/v1` rather than a one-file `static-files/v1`.
/// The two describe the same artifact; the narrower one is a better answer
/// because it promises less and can therefore be relied on more.
pub fn select_preset(evidence: &DetectorEvidence) -> Result<AppPreset, PresetMismatch> {
    let meaningful: Vec<&String> = evidence
        .present_files
        .iter()
        .filter(|name| !is_incidental(name))
        .collect();

    let html_files: Vec<&&String> = meaningful
        .iter()
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            lower.ends_with(".html") || lower.ends_with(".htm")
        })
        .collect();

    // ── single-html/v1 ──────────────────────────────────────────────────────
    // One HTML file and nothing else that matters. The file need not be called
    // `index.html`: `expense.html` is what a person exports from an editor, and
    // requiring the rename would be Ato asking to be accommodated.
    if meaningful.len() == 1 && html_files.len() == 1 {
        return Ok(AppPreset::SingleHtml);
    }

    let node = evidence.node.as_ref();
    let has_package_json = node.is_some_and(|node| node.has_package_json);

    // ── node-static/v1 ──────────────────────────────────────────────────────
    // Deliberately narrow: package.json, a lockfile npm can use, and a build
    // script. Everything else about the project — which framework, which
    // bundler — is none of Ato's business, and asking would start the
    // inference machine over.
    if has_package_json {
        let node = node.expect("has_package_json implies node evidence");
        if !node.has_package_lock && !node.has_npm_shrinkwrap {
            return Err(PresetMismatch::new(
                "preset_node_static_needs_lockfile",
                "A built web app needs a package-lock.json so the build is \
                 reproducible. Run `npm install` once and include the lockfile.",
            ));
        }
        if node.script_build.is_none() {
            return Err(PresetMismatch::new(
                "preset_node_static_needs_build_script",
                "A built web app needs a `build` script in package.json. Ato \
                 runs `npm run build` and publishes what it writes to `dist/`.",
            ));
        }
        return Ok(AppPreset::NodeStatic);
    }

    // ── static-files/v1 ─────────────────────────────────────────────────────
    // A tree that is already a website. The entry must be `index.html`: with
    // several HTML files there is no non-arbitrary way to pick one, and
    // guessing would make the published site depend on our tie-break.
    if has(evidence, CANONICAL_ENTRY) {
        return Ok(AppPreset::StaticFiles);
    }

    if html_files.len() > 1 {
        return Err(PresetMismatch::new(
            "preset_static_files_needs_index",
            "This looks like a website with several pages, but there is no \
             index.html at the top level. Rename the page you want opened \
             first to index.html.",
        ));
    }

    Err(PresetMismatch::new(
        "preset_no_match",
        "Ato can turn an HTML file, a folder with an index.html, or a project \
         that builds to `dist/` into an App. This source is none of those yet.",
    ))
}

/// The authored overrides a preset expands to.
///
/// Expanding into the SAME override vocabulary the intent compiler already
/// reads is what keeps a preset from becoming a second way to describe an App.
/// A preset is a short name for a set of decisions somebody would otherwise
/// have to make one at a time — not a parallel configuration language.
pub fn preset_overrides(preset: AppPreset) -> Vec<(&'static str, String)> {
    match preset {
        AppPreset::SingleHtml | AppPreset::StaticFiles => vec![
            ("lane", "static_web".to_owned()),
            // The source tree IS the site. Both build-free presets publish from
            // the root; `single-html/v1` differs only in that its root holds
            // exactly one file, which the Source adapter has already renamed.
            ("static.output_root", String::new()),
            ("static.entry_path", CANONICAL_ENTRY.to_owned()),
            ("static.build", "none".to_owned()),
            // A single page has nowhere to route to; a multi-page site would
            // have its own files at those paths. Neither wants a fallback that
            // silently serves the entry document for a typo'd URL.
            ("static.spa_fallback", "false".to_owned()),
        ],
        AppPreset::NodeStatic => vec![
            ("lane", "static_web".to_owned()),
            ("static.build", "required".to_owned()),
            ("static.output_root", NODE_STATIC_OUTPUT_ROOT.to_owned()),
            ("static.entry_path", CANONICAL_ENTRY.to_owned()),
            // A built single-page app is the case where a fallback is usually
            // right, and the one place these presets differ on it.
            ("static.spa_fallback", "true".to_owned()),
        ],
    }
}

// ─────────────────────────────────────────────────── the Preset as a synthesizer
//
// A Preset is not a lane and not a second way to describe an App. It is a
// synthesizer that produces the SAME pair — a Contract draft and a Derivation
// draft — that an author would have written by hand for a source simple enough
// to describe without help. Downstream, nothing can tell which frontend a draft
// came through, and a hand-written `capsule.toml` that says the same things
// canonicalizes to the same ContractRef and the same DerivationRef.

use crate::authoring::{
    AuthoringDraft, AuthoringProvenance, BROWSER_PROTOCOL, ContractDraft, DerivationDraft,
    EffectClass, HTTP_PROTOCOL, HttpRequirement, InputDraft, InputIdentityRequirement,
    ObservationDraft, Observed, PortDraft, RequirementDraft, StepDraft, WORKSPACE_PROTOCOL,
};

/// The input id a Preset gives the source tree.
pub const PRESET_INPUT_ID: &str = "workspace";
/// The step id a Preset gives the served surface.
pub const PRESET_SERVE_STEP_ID: &str = "site";
/// The port id a Preset exports.
pub const PRESET_PORT_ID: &str = "app.http";
/// The observation ids a Preset's contract carries.
pub const PRESET_ROOT_OBSERVATION_ID: &str = "root";
pub const PRESET_SOURCE_OBSERVATION_ID: &str = "source-identity";

/// Synthesize the authoring pair for a source that fits a Preset.
///
/// ## Why the Contract is conservative
///
/// It observes the SOURCE's identity as well as "GET / is 200". Without that,
/// every static page in the world that answers 200 would be the same Capsule —
/// two unrelated uploads would collide onto one identity and one person's App
/// could be resumed as another's. An author who genuinely wants that weak an
/// identity can write it down; Ato must not choose it on their behalf.
///
/// The reverse matters just as much: Ato must never STRENGTHEN a Contract an
/// author wrote. Source identity is added here because nobody stated an
/// intent, not as a floor under everyone's.
///
/// System security policy — which files are never packed, what execution is
/// admitted — is enforced elsewhere and is deliberately absent from `K`. It is
/// not something the author chose to preserve, and putting it in the digest
/// would make a policy change rewrite every Capsule identity.
pub fn synthesize_authoring(preset: AppPreset) -> AuthoringDraft {
    let (root, spa_fallback) = match preset {
        AppPreset::SingleHtml | AppPreset::StaticFiles => (None, false),
        AppPreset::NodeStatic => (Some(NODE_STATIC_OUTPUT_ROOT.to_owned()), true),
    };
    AuthoringDraft {
        contract: ContractDraft {
            requirements: vec![
                ObservationDraft {
                    id: PRESET_ROOT_OBSERVATION_ID.to_owned(),
                    requirement: RequirementDraft::Http(HttpRequirement {
                        port: PRESET_PORT_ID.to_owned(),
                        method: "GET".to_owned(),
                        path: "/".to_owned(),
                        status: 200,
                        body_digest: None,
                    }),
                },
                ObservationDraft {
                    id: PRESET_SOURCE_OBSERVATION_ID.to_owned(),
                    requirement: RequirementDraft::InputIdentity(InputIdentityRequirement {
                        input: PRESET_INPUT_ID.to_owned(),
                        digest: Observed::Capture,
                    }),
                },
            ],
        },
        derivation: DerivationDraft {
            inputs: vec![InputDraft {
                id: PRESET_INPUT_ID.to_owned(),
                protocol: WORKSPACE_PROTOCOL.to_owned(),
                path: ".".to_owned(),
            }],
            runtimes: vec![],
            steps: vec![StepDraft {
                id: PRESET_SERVE_STEP_ID.to_owned(),
                protocol: BROWSER_PROTOCOL.to_owned(),
                op: "serve".to_owned(),
                argv: vec![],
                cwd: String::new(),
                env: Default::default(),
                source: Some(PRESET_INPUT_ID.to_owned()),
                root: root.clone(),
                entry: Some(CANONICAL_ENTRY.to_owned()),
                spa_fallback: Some(spa_fallback),
            }],
            ports: vec![PortDraft {
                id: PRESET_PORT_ID.to_owned(),
                protocol: HTTP_PROTOCOL.to_owned(),
                from: PRESET_SERVE_STEP_ID.to_owned(),
                guest_port: None,
            }],
            state: vec![],
            // `node-static/v1` builds before it serves. That step is not yet
            // expressible in `ato.capsule/1` — writing it out needs the
            // resolver that picks the package manager and the Node version,
            // which still lives in the execution projection — so it is carried
            // here rather than pretended into an argv nobody resolved.
            workspace_build: match preset {
                AppPreset::NodeStatic => Some(NODE_STATIC_OUTPUT_ROOT.to_owned()),
                _ => None,
            },
            // Serving files has no effect outside the continuation. A build
            // that installs from a registry still has none that outlives it.
            effects: EffectClass::Pure,
        },
        provenance: AuthoringProvenance::PresetSynthesized {
            preset: preset.id(),
        },
    }
}
