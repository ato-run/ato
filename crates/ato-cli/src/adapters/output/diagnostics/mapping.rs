use anyhow::Error as AnyhowError;
use capsule_core::execution_plan::error::AtoExecutionError;

use crate::application::pipeline::cleanup::PipelineAttemptError;

use crate::error_codes;

use super::heuristics::{
    collect_causes, detect_field, is_distributable_artifact_missing, is_entrypoint_issue,
    is_manifest_parse, is_manual_intervention_issue, is_publish_version_exists_conflict,
    is_required_field_issue, is_source_registration_issue, json_string_field,
};
use super::types::{CliDiagnostic, CliDiagnosticCode, CommandContext};

pub fn detect_command_context(args: &[String]) -> CommandContext {
    let mut i = 1usize;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--nacelle" {
            i += 2;
            continue;
        }
        if arg.starts_with("--nacelle=") || arg.starts_with('-') {
            i += 1;
            continue;
        }
        return match arg {
            "build" | "pack" => CommandContext::Build,
            "run" => CommandContext::Run,
            "publish" => CommandContext::Publish,
            "source" => CommandContext::Source,
            _ => CommandContext::Other,
        };
    }
    CommandContext::Other
}

pub fn from_anyhow(err: &AnyhowError, command_context: CommandContext) -> CliDiagnostic {
    if let Some(attempt_err) = err.downcast_ref::<PipelineAttemptError>() {
        return from_anyhow(attempt_err.source_error(), command_context).with_cleanup(
            Some(attempt_err.cleanup_report().status),
            attempt_err.cleanup_report().actions.clone(),
        );
    }

    let causes = collect_causes(err);
    if let Some(execution_err) = err.downcast_ref::<AtoExecutionError>() {
        return from_execution_error(execution_err, causes);
    }

    if let Some(core_err) = err.downcast_ref::<capsule_core::CapsuleError>() {
        return from_capsule_error(core_err, causes);
    }

    let message = err.to_string();
    // OCI recipe needs a container engine but the user disabled Podman in
    // settings. Surface an actionable engine-missing diagnostic (E205) with
    // the clean message from `OciProviderError::PodmanDisabled` rather than
    // letting the wrapped error fall through to the generic E999 fallback.
    if err.chain().any(|cause| {
        cause
            .to_string()
            .contains("Podman is disabled in Ato settings")
    }) {
        return CliDiagnostic::new(
            CliDiagnosticCode::E205,
            "This recipe needs a container runtime, but Podman is disabled in Ato settings. \
             Enable Podman in Settings, then try again."
                .to_string(),
            Some("Ato の Settings で Podman を有効化してから、もう一度実行してください。"),
            None,
            None,
            None,
            false,
            true,
            causes,
        );
    }
    // Source-native build / prestart hook needs a host POSIX shell (/bin/sh)
    // that this platform (Windows without Git Bash/MSYS2) does not provide.
    // Surface a typed, actionable diagnostic (E213) instead of letting the
    // opaque "os error 2" spawn failure fall through to the generic E999. See
    // issue #377.
    if err.chain().any(|cause| {
        cause
            .to_string()
            .contains(crate::application::shell_preflight::SOURCE_BUILD_SHELL_UNAVAILABLE_MARKER)
    }) {
        return CliDiagnostic::new(
            CliDiagnosticCode::E213,
            message,
            Some(
                "このリポジトリは source-build 経路で Unix shell (/bin/sh) を必要とします。\
                 登録済み recipe があればそれを使う (OCI/runtime 経路で起動でき shell 不要)、\
                 無ければ Windows 対応の build script (PowerShell/cmd) を追加するか、\
                 Linux/macOS もしくは WSL 上で実行してください。",
            ),
            None,
            None,
            None,
            false,
            false,
            causes,
        );
    }
    // The app's own dependency build needs the `git` CLI to fetch a git-URL
    // dependency (e.g. `pkg @ git+https://…` in requirements.txt/uv.lock, or a
    // `git+https://…` entry in package.json), but `git` is not installed on the
    // host. Ato's GitHub *source fetch* is gitless (tarball API), so this is the
    // app build toolchain — not Ato — requiring git. Surface a typed, actionable
    // E203 (dependency_install_failed) instead of letting the opaque uv/npm
    // "git not found" error fall through to the generic E999. See the gitless
    // GitHub source install hotfix.
    if is_build_toolchain_git_missing(err) {
        return CliDiagnostic::new(
            CliDiagnosticCode::E203,
            "This app's dependency build needs the `git` CLI to fetch a git-URL \
             dependency (e.g. `git+https://…`), but `git` is not installed. Ato's \
             GitHub source fetch itself does not need git; this requirement comes \
             from the app's own dependencies. Install git and retry."
                .to_string(),
            Some(
                "このアプリの依存解決が git-URL 依存 (例: `git+https://…`) を取得するために \
                 `git` CLI を必要としていますが、git が見つかりません。Ato の GitHub ソース取得 \
                 自体は git 不要ですが、アプリ側の依存が git を要求しています。git を \
                 インストールしてから再実行してください。",
            ),
            None,
            None,
            None,
            true,
            false,
            causes,
        );
    }
    // A container started but exited before passing its readiness probe. Both
    // the multi-service executor and the orchestration session path preserve a
    // typed `OciExitedBeforeReadyError` in the chain; classify it as the typed
    // `oci_container_exited_before_ready` diagnostic (carrying service name,
    // exit code, and a log tail) instead of folding it into the generic E999
    // fallback. See #445 / #429.
    if let Some(diagnostic) = exited_before_ready_diagnostic(err, &causes) {
        return diagnostic;
    }
    if let Some(artifact_message) = distributable_artifact_missing_message(err) {
        return CliDiagnostic::new(
            CliDiagnosticCode::E102,
            artifact_message,
            Some(
                "配布可能な成果物が見つかりません。packaged build script が .app / .exe / .AppImage を生成するか確認し、必要なら contract.delivery.artifact.path を実際の出力先に合わせて更新してください。",
            ),
            None,
            Some("contract.delivery.artifact.path"),
            None,
            false,
            true,
            causes,
        );
    }
    if message.contains(error_codes::ATO_ERR_AUTH_REQUIRED) {
        return CliDiagnostic::new(
            CliDiagnosticCode::E201,
            message,
            Some(
                "`ato login`、`ato login --headless`、または `ATO_TOKEN=<token>` を使って再試行してください。",
            ),
            None,
            None,
            None,
            true,
            true,
            causes,
        );
    }
    if message.contains(error_codes::ATO_ERR_INTEGRITY_FAILURE) {
        return CliDiagnostic::new(
            CliDiagnosticCode::E208,
            message,
            Some("artifact や registry 応答の整合性を確認し、必要なら再取得してください。"),
            None,
            None,
            None,
            true,
            false,
            causes,
        );
    }
    if message.contains("ingress TLS bootstrap required") {
        return CliDiagnostic::new(
            CliDiagnosticCode::E209,
            message,
            Some(
                "`ato binding bootstrap-tls --binding <binding-id> [--install-system-trust]` を実行して明示的に TLS をセットアップしてください。",
            ),
            None,
            None,
            None,
            false,
            true,
            causes,
        );
    }
    if message.contains("ingress TLS bootstrap requires explicit consent")
        || message.contains("ingress TLS trust installation failed")
        || message.contains("ingress TLS bootstrap cancelled")
    {
        return CliDiagnostic::new(
            CliDiagnosticCode::E210,
            message,
            Some(
                "TLS trust bootstrap は明示的同意が必要です。内容を確認して `ato binding bootstrap-tls --binding <binding-id> --install-system-trust --yes` を再実行してください。",
            ),
            None,
            None,
            None,
            true,
            true,
            causes,
        );
    }
    if is_manifest_parse(&message) {
        return CliDiagnostic::new(
            CliDiagnosticCode::E001,
            message,
            Some("capsule.toml の TOML 構文を確認してください。"),
            None,
            None,
            None,
            false,
            false,
            causes,
        );
    }

    if is_required_field_issue(&message) {
        let field = detect_field(&message);
        return CliDiagnostic::new(
            CliDiagnosticCode::E003,
            message,
            Some("必須項目 (default_target / targets.<label>) を追加してください。"),
            None,
            field,
            None,
            false,
            false,
            causes,
        );
    }

    if is_entrypoint_issue(&message) {
        return CliDiagnostic::new(
            CliDiagnosticCode::E101,
            message,
            Some(
                "entrypoint のパスがプロジェクトルートか source/ 配下に存在するか確認してください。",
            ),
            None,
            Some("targets.<label>.entrypoint"),
            None,
            false,
            false,
            causes,
        );
    }

    if is_manual_intervention_issue(&message) {
        return CliDiagnostic::new(
            CliDiagnosticCode::E102,
            message,
            Some(
                "生成された capsule.toml と必要な環境変数・外部依存を確認し、準備後に再実行してください。",
            ),
            None,
            None,
            None,
            false,
            true,
            causes,
        );
    }

    if matches!(
        command_context,
        CommandContext::Publish | CommandContext::Source
    ) && is_source_registration_issue(&message)
    {
        return CliDiagnostic::new(
            CliDiagnosticCode::E201,
            message,
            Some("`ato login` で認証後、GitHub リポジトリへのアクセス権限を確認してください。"),
            None,
            None,
            None,
            true,
            true,
            causes,
        );
    }

    if matches!(command_context, CommandContext::Publish) {
        if let Some(crate::publish_artifact::PublishArtifactError::VersionExists { message }) =
            err.downcast_ref::<crate::publish_artifact::PublishArtifactError>()
        {
            return CliDiagnostic::new(
                CliDiagnosticCode::E202,
                message.clone(),
                Some(
                    "同じ version が既に存在します。version を上げるか、同一内容なら --allow-existing を使用してください。必要に応じてローカル registry を初期化してください。",
                ),
                None,
                None,
                None,
                true,
                false,
                causes,
            );
        }

        if let Some(crate::publish_artifact::PublishArtifactError::ManagedStoreLargePayloadOverrideUnsupported { message, .. }) =
            err.downcast_ref::<crate::publish_artifact::PublishArtifactError>()
        {
            return CliDiagnostic::new(
                CliDiagnosticCode::E212,
                message.clone(),
                Some(
                    "managed Store direct publish では large payload override は使えません。private/local registry を使うか、presigned upload 対応後に再試行してください。",
                ),
                None,
                None,
                None,
                false,
                false,
                causes,
            );
        }

        if let Some(
            crate::publish_artifact::PublishArtifactError::ManagedStoreDirectPayloadLimitExceeded {
                registry_url,
                size_bytes,
                limit_bytes,
            },
        ) = err.downcast_ref::<crate::publish_artifact::PublishArtifactError>()
        {
            return CliDiagnostic::new(
                CliDiagnosticCode::E212,
                format!(
                    "managed Store direct publish currently rejects artifacts larger than the conservative preflight limit: artifact is {} bytes, limit is {} bytes, destination is {}",
                    size_bytes, limit_bytes, registry_url
                ),
                Some(
                    "artifact を小さくするか、private/local registry を使ってください。official direct upload path は presigned upload 対応までこの制限を維持します。",
                ),
                None,
                None,
                None,
                false,
                false,
                causes,
            );
        }

        if let Some(crate::publish_artifact::PublishArtifactError::PayloadTooLarge {
            message,
            ..
        }) = err.downcast_ref::<crate::publish_artifact::PublishArtifactError>()
        {
            return CliDiagnostic::new(
                CliDiagnosticCode::E212,
                message.clone(),
                Some(
                    "artifact が managed Store direct upload path の上限を超えました。private/local registry を使うか、presigned upload 対応後に再試行してください。",
                ),
                None,
                None,
                None,
                true,
                false,
                causes,
            );
        }

        if is_publish_version_exists_conflict(&message) {
            return CliDiagnostic::new(
                CliDiagnosticCode::E202,
                message,
                Some(
                    "同じ version が既に存在します。version を上げるか、同一内容なら --allow-existing を使用してください。必要に応じてローカル registry を初期化してください。",
                ),
                None,
                None,
                None,
                true,
                false,
                causes,
            );
        }
    }

    CliDiagnostic::new(
        CliDiagnosticCode::E999,
        message,
        Some(
            "Run with RUST_BACKTRACE=1 for a full trace. If this problem persists, please file a bug.",
        ),
        None,
        None,
        None,
        true,
        false,
        causes,
    )
}

fn distributable_artifact_missing_message(err: &AnyhowError) -> Option<String> {
    err.chain()
        .map(|cause| cause.to_string())
        .find(|message| is_distributable_artifact_missing(message))
}

/// True when the error chain shows the app's dependency build failed because the
/// host has no `git` CLI to fetch a git-URL dependency.
///
/// Matches the package-manager signatures observed when `git` is absent from
/// PATH during dependency resolution:
/// - uv:  "Git executable not found. Ensure that Git is installed and available."
/// - pip: "Cannot find command 'git'"
/// - npm: spawning `git` fails with ENOENT ("spawn git" + "enoent")
///
/// Ato's own GitHub source fetch is tarball-based (gitless), so a git requirement
/// here always originates from the app's declared dependencies, not from Ato.
pub(super) fn is_build_toolchain_git_missing(err: &AnyhowError) -> bool {
    err.chain().any(|cause| {
        // Lowercase once so casing variants of the package-manager signatures
        // (e.g. real npm emits uppercase `spawn git ENOENT`) still match.
        let lower = cause.to_string().to_lowercase();
        lower.contains("git executable not found")
            || lower.contains("cannot find command 'git'")
            || (lower.contains("spawn git") && lower.contains("enoent"))
    })
}

/// Map an exited-before-ready failure (a container that started but died before
/// passing its readiness probe) to the typed `oci_container_exited_before_ready`
/// diagnostic (E306), preserving the service name, exit code, and log tail.
///
/// Prefers the structured typed error carried in the chain; falls back to the
/// textual marker so any path that emits it still avoids the E999 fallback.
fn exited_before_ready_diagnostic(err: &AnyhowError, causes: &[String]) -> Option<CliDiagnostic> {
    use crate::adapters::runtime::executors::oci_multi_service::{
        OciExitedBeforeReadyError, OCI_EXITED_BEFORE_READY_CODE,
    };

    const HINT: &str = "コンテナは起動しましたが readiness 前に終了しました。上のログを確認してください。\
         state mount で permission denied が出ている場合は、state の ownership / mount 方式を見直してください。";

    if let Some(typed) = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<OciExitedBeforeReadyError>())
    {
        let details = serde_json::json!({
            "service": typed.service_name,
            "exit_code": typed.exit_code,
            "last_logs": typed.log_tail,
        });
        return Some(CliDiagnostic::new(
            CliDiagnosticCode::E306,
            typed.to_string(),
            Some(HINT),
            None,
            None,
            Some(details),
            false,
            false,
            causes.to_vec(),
        ));
    }

    // Defensive fallback: a chain entry carries the textual marker without the
    // typed error (e.g. a future call site). Still classify as E306, not E999.
    if err
        .chain()
        .any(|cause| cause.to_string().contains(OCI_EXITED_BEFORE_READY_CODE))
    {
        let message = err
            .chain()
            .map(|cause| cause.to_string())
            .find(|m| m.contains(OCI_EXITED_BEFORE_READY_CODE))
            .unwrap_or_else(|| err.to_string());
        return Some(CliDiagnostic::new(
            CliDiagnosticCode::E306,
            message,
            Some(HINT),
            None,
            None,
            None,
            false,
            false,
            causes.to_vec(),
        ));
    }

    None
}

pub fn map_exit_code(diagnostic: &CliDiagnostic, err: &AnyhowError) -> i32 {
    if let Some(core_err) = err.downcast_ref::<capsule_core::CapsuleError>() {
        return match core_err {
            capsule_core::CapsuleError::Network(_) => error_codes::EXIT_NETWORK_ERROR,
            capsule_core::CapsuleError::ContainerEngine(_)
            | capsule_core::CapsuleError::Runtime(_)
            | capsule_core::CapsuleError::ProcessStart(_)
            | capsule_core::CapsuleError::Timeout => error_codes::EXIT_RUNTIME_ERROR,
            _ => code_to_exit(diagnostic.code),
        };
    }

    if err
        .chain()
        .any(|source| source.downcast_ref::<reqwest::Error>().is_some())
    {
        return error_codes::EXIT_NETWORK_ERROR;
    }

    code_to_exit(diagnostic.code)
}

fn code_to_exit(code: CliDiagnosticCode) -> i32 {
    match code {
        CliDiagnosticCode::E305 | CliDiagnosticCode::E306 => error_codes::EXIT_RUNTIME_ERROR,
        CliDiagnosticCode::E212 => error_codes::EXIT_USER_ERROR,
        CliDiagnosticCode::E999 => error_codes::EXIT_SYSTEM_ERROR,
        _ => error_codes::EXIT_USER_ERROR,
    }
}

fn from_execution_error(execution_err: &AtoExecutionError, causes: Vec<String>) -> CliDiagnostic {
    let code = map_execution_code(execution_err.code);
    CliDiagnostic::new(
        code,
        execution_err.message.clone(),
        execution_err.hint.as_deref(),
        None,
        json_string_field(execution_err.details.as_ref(), "field"),
        execution_err.details.clone(),
        execution_err.retryable,
        execution_err.interactive_resolution,
        causes,
    )
    .with_classification(execution_err.classification)
    .with_cleanup(
        execution_err.cleanup_status,
        execution_err.cleanup_actions.clone(),
    )
    .with_manifest_suggestion(execution_err.manifest_suggestion.clone())
}

fn map_execution_code(code: &str) -> CliDiagnosticCode {
    match code {
        "ATO_ERR_MANUAL_INTERVENTION_REQUIRED" => CliDiagnosticCode::E102,
        "ATO_ERR_MISSING_REQUIRED_ENV" => CliDiagnosticCode::E103,
        "ATO_ERR_AMBIGUOUS_ENTRYPOINT" => CliDiagnosticCode::E105,
        "ATO_ERR_SECURITY_POLICY_VIOLATION" => CliDiagnosticCode::E301,
        "ATO_ERR_EXECUTION_CONTRACT_INVALID" => CliDiagnosticCode::E302,
        "ATO_ERR_RUNTIME_NOT_RESOLVED" => CliDiagnosticCode::E303,
        "ATO_ERR_ENGINE_MISSING" => CliDiagnosticCode::E205,
        "ATO_ERR_SKILL_NOT_FOUND" => CliDiagnosticCode::E206,
        "ATO_ERR_PROVISIONING_LOCK_INCOMPLETE" => CliDiagnosticCode::E104,
        "ATO_ERR_PROVISIONING_TLS_TRUST" => CliDiagnosticCode::E210,
        "ATO_ERR_PROVISIONING_TLS_BOOTSTRAP_REQUIRED" => CliDiagnosticCode::E209,
        "ATO_ERR_STORAGE_NO_SPACE" => CliDiagnosticCode::E211,
        "ATO_ERR_COMPAT_HARDWARE" => CliDiagnosticCode::E304,
        "ATO_ERR_ARTIFACT_INTEGRITY_FAILURE" => CliDiagnosticCode::E208,
        "ATO_ERR_RUNTIME_LAUNCH_FAILED" => CliDiagnosticCode::E305,
        "ATO_ERR_LOCKFILE_TAMPERED" => CliDiagnosticCode::E207,
        "ATO_ERR_POLICY_VIOLATION" => CliDiagnosticCode::E301,
        _ => CliDiagnosticCode::E999,
    }
}

fn from_capsule_error(core_err: &capsule_core::CapsuleError, causes: Vec<String>) -> CliDiagnostic {
    match core_err {
        capsule_core::CapsuleError::Manifest(path, detail) => {
            if is_manifest_parse(detail) {
                return CliDiagnostic::new(
                    CliDiagnosticCode::E001,
                    detail,
                    Some("capsule.toml の TOML 構文を確認してください。"),
                    Some(path.as_path()),
                    None,
                    None,
                    false,
                    false,
                    causes,
                );
            }
            if is_required_field_issue(detail) {
                return CliDiagnostic::new(
                    CliDiagnosticCode::E003,
                    detail,
                    Some("必須項目 (default_target / targets.<label>) を追加してください。"),
                    Some(path.as_path()),
                    detect_field(detail),
                    None,
                    false,
                    false,
                    causes,
                );
            }
            CliDiagnostic::new(
                CliDiagnosticCode::E002,
                detail,
                Some("schema_version=0.2 と Manifest スキーマの整合性を確認してください。"),
                Some(path.as_path()),
                detect_field(detail),
                None,
                false,
                false,
                causes,
            )
        }
        capsule_core::CapsuleError::Pack(detail) => {
            if is_entrypoint_issue(detail) {
                return CliDiagnostic::new(
                    CliDiagnosticCode::E101,
                    detail,
                    Some(
                        "entrypoint のパスがプロジェクトルートか source/ 配下に存在するか確認してください。",
                    ),
                    None,
                    Some("targets.<label>.entrypoint"),
                    None,
                    false,
                    false,
                    causes,
                );
            }
            CliDiagnostic::new(
                CliDiagnosticCode::E102,
                detail,
                Some("build 設定・依存関係を確認し、必要に応じてコマンドを再実行してください。"),
                None,
                None,
                None,
                false,
                true,
                causes,
            )
        }
        capsule_core::CapsuleError::StrictManifestFallbackNotAllowed(detail) => CliDiagnostic::new(
            CliDiagnosticCode::E106,
            detail,
            Some(
                "--strict-v3 を無効化するか、source_digest をCASに登録して manifest 経路を成功させてください。",
            ),
            None,
            Some("strict-v3"),
            None,
            false,
            false,
            causes,
        ),
        capsule_core::CapsuleError::AuthRequired(detail) => CliDiagnostic::new(
            CliDiagnosticCode::E201,
            format!("Authentication required: {}", detail),
            Some("`ato login` を実行して認証情報を設定してください。"),
            None,
            None,
            None,
            true,
            true,
            causes,
        ),
        other => CliDiagnostic::new(
            CliDiagnosticCode::E999,
            other.to_string(),
            Some(
                "Run with RUST_BACKTRACE=1 for a full trace. If this problem persists, please file a bug.",
            ),
            None,
            None,
            None,
            true,
            false,
            causes,
        ),
    }
}

#[cfg(test)]
mod podman_disabled_tests {
    use anyhow::Context as _;

    use super::{from_anyhow, CliDiagnosticCode, CommandContext};

    #[test]
    fn smoke_shell_failure_maps_to_e213() {
        // The smoke runner reports a SpawnFailed whose message carries the
        // shared marker when `executable = "sh"` and no host shell exists. That
        // must reach E213 through from_anyhow, not the generic E999. (#377)
        let report = capsule_core::smoke::SmokeFailureReport {
            class: capsule_core::smoke::SmokeFailureClass::SpawnFailed,
            message: capsule_core::shell_support::source_build_shell_unavailable_message(
                "sh", "windows",
            ),
            stderr_tail: String::new(),
            exit_status: None,
        };
        let err = anyhow::Error::new(report).context("Smoke test failed");

        let diagnostic = from_anyhow(&err, CommandContext::Build);
        assert_eq!(
            diagnostic.code,
            CliDiagnosticCode::E213,
            "smoke shell failure must map to E213"
        );
    }

    #[test]
    fn smoke_non_shell_spawn_failure_does_not_map_to_e213() {
        // A non-shell SpawnFailed (e.g. a missing binary) carries no marker and
        // must NOT be reclassified as the shell-unavailable error.
        let report = capsule_core::smoke::SmokeFailureReport {
            class: capsule_core::smoke::SmokeFailureClass::SpawnFailed,
            message: "failed to start process 'node' for smoke: No such file or directory"
                .to_string(),
            stderr_tail: String::new(),
            exit_status: None,
        };
        let err = anyhow::Error::new(report);

        let diagnostic = from_anyhow(&err, CommandContext::Build);
        assert_ne!(
            diagnostic.code,
            CliDiagnosticCode::E213,
            "non-shell spawn failures must not be reclassified as shell-unavailable"
        );
    }

    #[test]
    fn source_build_shell_unavailable_maps_to_e213_not_internal() {
        // Mirrors the real wrapping: the prestart spawn site adds context on
        // top of the typed shell-unavailable error from `shell_preflight`.
        let err = crate::application::shell_preflight::source_build_shell_unavailable_error(
            "cd app && bun install",
            "windows",
        )
        .context("running prestart command");

        let diagnostic = from_anyhow(&err, CommandContext::Run);
        assert_eq!(
            diagnostic.code,
            CliDiagnosticCode::E213,
            "shell-unavailable must map to source_build_shell_unavailable (E213), not E999"
        );
        assert_eq!(diagnostic.name, "source_build_shell_unavailable");
        assert!(
            diagnostic.hint.is_some(),
            "diagnostic must carry an actionable hint"
        );
    }

    #[test]
    fn uv_git_missing_during_build_maps_to_e203_not_internal() {
        // Real uv signature when a `git+https://…` dependency is resolved on a
        // host without the git CLI. Ato's source fetch is gitless, so this is the
        // app's build toolchain requiring git: surface a typed, actionable E203
        // instead of the opaque E999 fallback a clean-VM user currently hits.
        let err = anyhow::anyhow!(
            "Git operation failed: Git executable not found. Ensure that Git is installed and available."
        )
        .context("failed to materialize dependencies for source-python run");

        let diagnostic = from_anyhow(&err, CommandContext::Run);
        assert_eq!(
            diagnostic.code,
            CliDiagnosticCode::E203,
            "uv git-missing build failure must map to dependency_install_failed (E203), not E999"
        );
        assert_eq!(diagnostic.name, "dependency_install_failed");
        assert!(
            diagnostic.message.contains("does not need git"),
            "diagnostic must clarify Ato's fetch is gitless, got: {}",
            diagnostic.message
        );
        assert!(
            diagnostic.hint.is_some(),
            "diagnostic must carry an actionable hint"
        );
    }

    #[test]
    fn npm_git_spawn_enoent_during_build_maps_to_e203() {
        // Real npm signature when a `git+https://…` dependency is installed
        // without the git CLI: spawning `git` fails with ENOENT.
        let err = anyhow::anyhow!(
            "npm error syscall spawn git\nnpm error enoent An unknown git error occurred"
        )
        .context("failed to materialize provider-backed npm package");

        let diagnostic = from_anyhow(&err, CommandContext::Run);
        assert_eq!(
            diagnostic.code,
            CliDiagnosticCode::E203,
            "npm git-spawn-ENOENT build failure must map to E203, not E999"
        );
    }

    #[test]
    fn npm_git_spawn_enoent_uppercase_during_build_maps_to_e203() {
        // npm emits the syscall error with uppercase ENOENT in practice; the
        // matcher must be case-insensitive so this still maps to E203, not E999.
        let err = anyhow::anyhow!("npm error syscall spawn git ENOENT")
            .context("failed to materialize provider-backed npm package");

        let diagnostic = from_anyhow(&err, CommandContext::Run);
        assert_eq!(
            diagnostic.code,
            CliDiagnosticCode::E203,
            "uppercase `spawn git ENOENT` build failure must map to E203, not E999"
        );
    }

    #[test]
    fn generic_dependency_failure_without_git_signature_does_not_map_to_e203() {
        // A dependency build failure unrelated to git must NOT be reclassified as
        // the git-missing diagnostic.
        let err = anyhow::anyhow!(
            "failed to install seed packages into virtual environment: No solution found"
        )
        .context("failed to materialize dependencies");

        let diagnostic = from_anyhow(&err, CommandContext::Run);
        assert_ne!(
            diagnostic.code,
            CliDiagnosticCode::E203,
            "non-git dependency failures must not be reclassified as git-missing"
        );
    }

    #[test]
    fn podman_disabled_maps_to_engine_missing_not_internal() {
        // Mirrors the real wrapping: the session-start path adds the
        // "OCI provider not ready before session start" context on top of the
        // `OciProviderError::PodmanDisabled` Display string.
        let err = anyhow::anyhow!(
            "This recipe needs a container runtime, but Podman is disabled in Ato settings. \
             Enable Podman in Settings, then try again."
        )
        .context("OCI provider not ready before session start");

        let diagnostic = from_anyhow(&err, CommandContext::Run);
        assert_eq!(
            diagnostic.code,
            CliDiagnosticCode::E205,
            "Podman-disabled must map to engine_missing (E205), not the E999 fallback"
        );
        assert!(
            diagnostic
                .message
                .contains("Podman is disabled in Ato settings"),
            "diagnostic must carry the actionable message, got: {}",
            diagnostic.message
        );
    }
}

#[cfg(test)]
mod exited_before_ready_tests {
    use super::{from_anyhow, CliDiagnosticCode, CommandContext};
    use crate::adapters::runtime::executors::oci_multi_service::OciExitedBeforeReadyError;

    fn db_exited_error() -> OciExitedBeforeReadyError {
        OciExitedBeforeReadyError {
            service_name: "db".to_string(),
            exit_code: Some(1),
            log_tail: vec![
                "initdb: error: could not change permissions of directory".to_string(),
                "chmod: /var/lib/postgresql/data: Operation not permitted".to_string(),
            ],
        }
    }

    #[test]
    fn multiservice_exit_before_ready_maps_to_e306_not_internal() {
        // Mirrors the real session-start wrapping: the orchestration path adds
        // the "orchestration services failed to start in-process" context on top
        // of the typed exited-before-ready error. It must reach E306, not the
        // generic E999 fallback. (#445)
        let err = anyhow::Error::new(db_exited_error())
            .context("orchestration services failed to start in-process");

        let diagnostic = from_anyhow(&err, CommandContext::Run);
        assert_eq!(
            diagnostic.code,
            CliDiagnosticCode::E306,
            "db exited-before-ready must map to E306, got {:?}: {}",
            diagnostic.code,
            diagnostic.message
        );
        assert_eq!(diagnostic.name, "oci_container_exited_before_ready");
        assert!(diagnostic.hint.is_some(), "diagnostic must carry a hint");
    }

    #[test]
    fn exit_before_ready_preserves_service_exit_code_and_log_tail() {
        let err = anyhow::Error::new(db_exited_error())
            .context("orchestration services failed to start in-process");

        let diagnostic = from_anyhow(&err, CommandContext::Run);

        // Human-readable message keeps the service name + exit code + logs.
        assert!(
            diagnostic.message.contains("service 'db'"),
            "service name lost: {}",
            diagnostic.message
        );
        assert!(
            diagnostic.message.contains("status 1"),
            "exit code lost: {}",
            diagnostic.message
        );
        assert!(
            diagnostic.message.contains("Operation not permitted"),
            "log tail lost: {}",
            diagnostic.message
        );

        // Structured details carry the same fields for machine consumers.
        let details = diagnostic.details.expect("E306 must carry details");
        assert_eq!(details["service"], "db");
        assert_eq!(details["exit_code"], 1);
        assert_eq!(
            details["last_logs"]
                .as_array()
                .expect("last_logs is an array")
                .len(),
            2
        );
    }

    #[test]
    fn unrelated_orchestration_error_stays_internal() {
        // A generic orchestration failure (not exited-before-ready) must keep the
        // existing E999 classification — we only reclassify the typed error.
        let err = anyhow::anyhow!("network bridge could not be created")
            .context("orchestration services failed to start in-process");

        let diagnostic = from_anyhow(&err, CommandContext::Run);
        assert_eq!(
            diagnostic.code,
            CliDiagnosticCode::E999,
            "unrelated orchestration errors must stay E999"
        );
    }

    #[test]
    fn local_service_exit_before_ready_stays_internal() {
        // A local (source/native/managed) service exits before readiness with the
        // generic orchestration string — it must NOT be reclassified as the
        // OCI-specific E306, since that diagnostic (and its hint) is OCI-only.
        let err =
            anyhow::anyhow!("service 'web' exited before readiness check passed (exit code: 1)")
                .context("orchestration services failed to start in-process");

        let diagnostic = from_anyhow(&err, CommandContext::Run);
        assert_ne!(
            diagnostic.code,
            CliDiagnosticCode::E306,
            "local service exit must not map to the OCI exited-before-ready code"
        );
        assert_eq!(
            diagnostic.code,
            CliDiagnosticCode::E999,
            "local service exit keeps the existing generic classification"
        );
    }

    #[test]
    fn healthcheck_timeout_does_not_map_to_e306() {
        // A readiness timeout (container still running) is a different, unchanged
        // diagnostic and must not be reclassified as exited-before-ready.
        let err = anyhow::anyhow!(
            "oci_healthcheck_timeout: service 'main' did not become ready within 30s"
        )
        .context("orchestration services failed to start in-process");

        let diagnostic = from_anyhow(&err, CommandContext::Run);
        assert_ne!(
            diagnostic.code,
            CliDiagnosticCode::E306,
            "healthcheck timeout must not be reclassified as exited-before-ready"
        );
    }
}
