use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Result;
use capsule_core::execution_plan::{derive, guard};
use capsule_core::router::ExecutionProfile;
use serde::Serialize;

use crate::application::producer_input::resolve_producer_authoritative_input;
use crate::publish_preflight::{self, CI_WORKFLOW_REL_PATH};

const MAIN_BRANCH: &str = "main";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishRouteKind {
    Official,
    Private,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublishRoutePlan {
    pub registry_url: String,
    pub route: PublishRouteKind,
}

#[derive(Debug, Clone, Serialize)]
pub struct StageResult {
    pub key: &'static str,
    pub ok: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnoseIssue {
    pub stage: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OfficialPublishDiagnosis {
    pub registry_url: String,
    pub route: PublishRouteKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capsule_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub stages: Vec<StageResult>,
    pub issues: Vec<DiagnoseIssue>,
    pub next_commands: Vec<String>,
    pub needs_workflow_fix: bool,
    pub can_handoff: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct WorkflowFixResult {
    pub attempted: bool,
    pub applied: bool,
    pub changed: bool,
    pub created: bool,
}

pub fn build_route_plan(registry_url: &str) -> PublishRoutePlan {
    PublishRoutePlan {
        registry_url: registry_url.to_string(),
        route: route_for_registry(registry_url),
    }
}

pub fn route_for_registry(url: &str) -> PublishRouteKind {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return PublishRouteKind::Private;
    };
    let Some(host) = parsed.host_str() else {
        return PublishRouteKind::Private;
    };
    if host.eq_ignore_ascii_case("api.ato.run") || host.eq_ignore_ascii_case("staging.api.ato.run")
    {
        PublishRouteKind::Official
    } else {
        PublishRouteKind::Private
    }
}

pub fn diagnose_official(cwd: &Path, registry_url: &str) -> OfficialPublishDiagnosis {
    let route = route_for_registry(registry_url);
    let mut stages = Vec::new();
    let mut issues = Vec::new();

    let mut capsule_name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut manifest_repo: Option<String> = None;

    let preflight_ok = match resolve_producer_authoritative_input(
        cwd,
        std::sync::Arc::new(crate::reporters::CliReporter::new(false)),
        false,
    ) {
        Ok(authoritative_input) => {
            let manifest_path = authoritative_input.descriptor.manifest_path.clone();
            capsule_name = authoritative_input.semantic_package_name().ok();
            version = Some(authoritative_input.semantic_package_version());
            manifest_repo = authoritative_input
                .compat_manifest
                .as_ref()
                .and_then(|bridge| bridge.repository());

            let compiled =
                derive::compile_execution_plan(&manifest_path, ExecutionProfile::Release, None);
            match compiled {
                Ok(compiled) => {
                    if let Err(err) = guard::evaluate(
                        &compiled.execution_plan,
                        &compiled.runtime_decision.plan.manifest_dir,
                        "strict",
                        true,
                        false,
                    ) {
                        issues.push(DiagnoseIssue {
                            stage: "preflight",
                            message: err.to_string(),
                            action: Some("ato build .".to_string()),
                        });
                        false
                    } else {
                        true
                    }
                }
                Err(err) => {
                    issues.push(DiagnoseIssue {
                        stage: "preflight",
                        message: err.to_string(),
                        action: Some("authoritative build metadata を修正".to_string()),
                    });
                    false
                }
            }
        }
        Err(err) => {
            issues.push(DiagnoseIssue {
                stage: "preflight",
                message: err.to_string(),
                action: Some("プロジェクトルートで authoritative input を解決できる状態にして `ato publish` を再実行".to_string()),
            });
            false
        }
    };
    stages.push(StageResult {
        key: "preflight",
        ok: preflight_ok,
        message: if preflight_ok {
            "manifest / lock / runtime guard is ready".to_string()
        } else {
            "manifest preflight failed".to_string()
        },
    });

    let mut needs_workflow_fix = false;
    let workflow_ok = match publish_preflight::validate_ci_workflow(cwd) {
        Ok(_) => true,
        Err(err) => {
            needs_workflow_fix = true;
            issues.push(DiagnoseIssue {
                stage: "workflow_check",
                message: err.to_string(),
                action: Some("ato publish --fix".to_string()),
            });
            false
        }
    };
    stages.push(StageResult {
        key: "workflow_check",
        ok: workflow_ok,
        message: if workflow_ok {
            format!("{} is valid", CI_WORKFLOW_REL_PATH)
        } else {
            "workflow check failed".to_string()
        },
    });

    let mut repository = manifest_repo.clone();
    let mut branch = None;
    let git_ok = match publish_preflight::run_git_checks(manifest_repo.as_deref()) {
        Ok(git) => {
            repository = git.origin.clone().or(manifest_repo.clone());
            if git.dirty {
                issues.push(DiagnoseIssue {
                    stage: "git_state",
                    message: "working tree is dirty".to_string(),
                    action: Some(
                        "git add -A && git commit -m \"chore: prepare publish\"".to_string(),
                    ),
                });
            }
            match publish_preflight::git_current_branch() {
                Ok(current) => {
                    branch = Some(current.clone());
                    if current != MAIN_BRANCH {
                        issues.push(DiagnoseIssue {
                            stage: "git_state",
                            message: format!(
                                "current branch is '{}' (expected '{}')",
                                current, MAIN_BRANCH
                            ),
                            action: Some(format!("git checkout {}", MAIN_BRANCH)),
                        });
                    }
                }
                Err(err) => {
                    issues.push(DiagnoseIssue {
                        stage: "git_state",
                        message: format!("failed to resolve current branch: {}", err),
                        action: Some("git branch --show-current".to_string()),
                    });
                }
            }
            true
        }
        Err(err) => {
            issues.push(DiagnoseIssue {
                stage: "git_state",
                message: err.to_string(),
                action: infer_git_fix_action(err.to_string(), manifest_repo.as_deref()),
            });
            false
        }
    };
    stages.push(StageResult {
        key: "git_state",
        ok: git_ok && !issues.iter().any(|issue| issue.stage == "git_state"),
        message: if git_ok {
            "repository checks completed".to_string()
        } else {
            "repository checks failed".to_string()
        },
    });

    let mut trigger_ok = true;
    let expected_tag = resolve_expected_tag(version.as_deref());
    if let Some(tag) = expected_tag.as_ref() {
        let tag_list = publish_preflight::run_git(&["tag", "--points-at", "HEAD"]);
        match tag_list {
            Ok(tags) => {
                let exists = tags.lines().any(|line| line.trim() == tag);
                if !exists {
                    trigger_ok = false;
                    issues.push(DiagnoseIssue {
                        stage: "ci_trigger",
                        message: format!("HEAD is not tagged with {}", tag),
                        action: Some(format!("git tag {tag} && git push origin {tag}")),
                    });
                }
            }
            Err(err) => {
                trigger_ok = false;
                issues.push(DiagnoseIssue {
                    stage: "ci_trigger",
                    message: format!("failed to inspect tags: {}", err),
                    action: Some(format!("git tag {tag} && git push origin {tag}")),
                });
            }
        }
    } else {
        trigger_ok = false;
        issues.push(DiagnoseIssue {
            stage: "ci_trigger",
            message: "version is unknown (cannot derive release tag)".to_string(),
            action: Some("capsule.toml の version を設定".to_string()),
        });
    }
    stages.push(StageResult {
        key: "ci_trigger",
        ok: trigger_ok,
        message: if trigger_ok {
            "CI trigger tag is ready".to_string()
        } else {
            "CI trigger precondition failed".to_string()
        },
    });

    let can_handoff = issues.is_empty();
    let next_commands = if can_handoff {
        let mut cmds = Vec::new();
        if let Some(tag) = expected_tag.as_ref() {
            cmds.push(format!("git tag {}", tag));
            cmds.push(format!("git push origin {}", tag));
        }
        cmds
    } else {
        collect_issue_actions(&issues)
    };

    OfficialPublishDiagnosis {
        registry_url: registry_url.to_string(),
        route,
        capsule_name,
        version,
        expected_tag,
        repository,
        branch,
        stages,
        issues,
        next_commands,
        needs_workflow_fix,
        can_handoff,
    }
}

fn resolve_expected_tag(version: Option<&str>) -> Option<String> {
    if let Some(version) = version.map(str::trim).filter(|value| !value.is_empty()) {
        return Some(format!("v{}", version));
    }

    let tags = publish_preflight::run_git(&["tag", "--points-at", "HEAD"]).ok()?;
    for tag in tags
        .lines()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let tag = tag.strip_prefix('v').unwrap_or(tag);
        if tag.split('.').count() == 3 {
            return Some(format!("v{}", tag));
        }
    }
    None
}

pub fn apply_workflow_fix_once(cwd: &Path) -> Result<WorkflowFixResult> {
    let outcome = crate::commands::gen_ci::sync_workflow_in_dir(cwd)?;
    Ok(WorkflowFixResult {
        attempted: true,
        applied: outcome.changed,
        changed: outcome.changed,
        created: outcome.created,
    })
}

pub fn collect_issue_actions(issues: &[DiagnoseIssue]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut actions = Vec::new();
    for action in issues.iter().filter_map(|issue| issue.action.as_ref()) {
        if seen.insert(action.clone()) {
            actions.push(action.clone());
        }
    }
    actions
}

fn infer_git_fix_action(message: String, manifest_repo: Option<&str>) -> Option<String> {
    let lower = message.to_ascii_lowercase();
    if lower.contains("not inside a git repository") {
        return Some("git init".to_string());
    }
    if lower.contains("remote origin") && lower.contains("missing") {
        if let Some(repo) = manifest_repo {
            return Some(format!("git remote add origin git@github.com:{repo}.git"));
        }
        return Some("git remote add origin git@github.com:<owner>/<repo>.git".to_string());
    }
    if lower.contains("repository mismatch") {
        if let Some(repo) = manifest_repo {
            return Some(format!(
                "git remote set-url origin git@github.com:{repo}.git"
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_for_registry_classifies_official_hosts() {
        assert_eq!(
            route_for_registry("https://api.ato.run"),
            PublishRouteKind::Official
        );
        assert_eq!(
            route_for_registry("https://staging.api.ato.run"),
            PublishRouteKind::Official
        );
        assert_eq!(
            route_for_registry("http://127.0.0.1:8787"),
            PublishRouteKind::Private
        );
    }

    #[test]
    fn collect_issue_actions_dedupes() {
        let issues = vec![
            DiagnoseIssue {
                stage: "a",
                message: "x".to_string(),
                action: Some("cmd1".to_string()),
            },
            DiagnoseIssue {
                stage: "b",
                message: "y".to_string(),
                action: Some("cmd1".to_string()),
            },
            DiagnoseIssue {
                stage: "c",
                message: "z".to_string(),
                action: Some("cmd2".to_string()),
            },
        ];

        let actions = collect_issue_actions(&issues);
        assert_eq!(actions, vec!["cmd1".to_string(), "cmd2".to_string()]);
    }
}
