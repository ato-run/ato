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
            Some("`ato login`、`ato login --headless`、または `ATO_TOKEN=<token>` を使って再試行してください。"),
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
            Some("`ato binding bootstrap-tls --binding <binding-id> [--install-system-trust]` を実行して明示的に TLS をセットアップしてください。"),
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
            Some("TLS trust bootstrap は明示的同意が必要です。内容を確認して `ato binding bootstrap-tls --binding <binding-id> --install-system-trust --yes` を再実行してください。"),
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
            Some("entrypoint のパスがプロジェクトルートか source/ 配下に存在するか確認してください。"),
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
            Some("生成された capsule.toml と必要な環境変数・外部依存を確認し、準備後に再実行してください。"),
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
        Some("再実行時に `RUST_BACKTRACE=1` を付けて詳細ログを確認してください。"),
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
        CliDiagnosticCode::E305 => error_codes::EXIT_RUNTIME_ERROR,
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
            Some("再実行時に `RUST_BACKTRACE=1` を付けて詳細ログを確認してください。"),
            None,
            None,
            None,
            true,
            false,
            causes,
        ),
    }
}
