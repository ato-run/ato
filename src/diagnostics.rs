use std::fmt;
use std::path::Path;

use anyhow::Error as AnyhowError;
use capsule_core::execution_plan::error::AtoExecutionError;
use miette::Diagnostic;
use serde::Serialize;
use thiserror::Error;

use crate::error_codes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandContext {
    Build,
    Run,
    Publish,
    Source,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliDiagnosticCode {
    E001,
    E002,
    E003,
    E101,
    E102,
    E201,
    E202,
    E999,
}

impl CliDiagnosticCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::E001 => "E001",
            Self::E002 => "E002",
            Self::E003 => "E003",
            Self::E101 => "E101",
            Self::E102 => "E102",
            Self::E201 => "E201",
            Self::E202 => "E202",
            Self::E999 => "E999",
        }
    }
}

impl fmt::Display for CliDiagnosticCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for CliDiagnosticCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Debug, Clone, Error, Serialize)]
#[error("{message}")]
pub struct CliDiagnostic {
    pub code: CliDiagnosticCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default)]
    pub causes: Vec<String>,
}

impl CliDiagnostic {
    fn new(
        code: CliDiagnosticCode,
        message: impl Into<String>,
        hint: Option<&str>,
        path: Option<&Path>,
        field: Option<&str>,
        causes: Vec<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            hint: hint.map(|v| v.to_string()),
            path: path.map(|v| v.display().to_string()),
            field: field.map(|v| v.to_string()),
            causes,
        }
    }

    pub fn to_json_envelope(&self) -> JsonErrorEnvelopeV1 {
        JsonErrorEnvelopeV1 {
            schema_version: "1",
            error_type: "error",
            code: self.code,
            message: self.message.clone(),
            hint: self.hint.clone(),
            path: self.path.clone(),
            field: self.field.clone(),
            causes: self.causes.clone(),
        }
    }
}

impl Diagnostic for CliDiagnostic {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.code))
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.hint
            .as_ref()
            .map(|v| Box::new(v.clone()) as Box<dyn fmt::Display>)
    }

    fn url<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        let url = format!(
            "https://ato.run/docs/errors#{}",
            self.code.as_str().to_ascii_lowercase()
        );
        Some(Box::new(url))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonErrorEnvelopeV1 {
    pub schema_version: &'static str,
    #[serde(rename = "type")]
    pub error_type: &'static str,
    pub code: CliDiagnosticCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default)]
    pub causes: Vec<String>,
}

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
            "run" | "open" => CommandContext::Run,
            "publish" => CommandContext::Publish,
            "source" => CommandContext::Source,
            _ => CommandContext::Other,
        };
    }
    CommandContext::Other
}

pub fn from_anyhow(err: &AnyhowError, command_context: CommandContext) -> CliDiagnostic {
    let causes = collect_causes(err);
    if let Some(execution_err) = err.downcast_ref::<AtoExecutionError>() {
        return CliDiagnostic::new(
            CliDiagnosticCode::E999,
            format!("{}: {}", execution_err.code, execution_err.message),
            execution_err.hint.as_deref().or(Some(
                "capsule.toml と runtime policy 設定を確認してください。",
            )),
            None,
            None,
            causes,
        );
    }

    if let Some(core_err) = err.downcast_ref::<capsule_core::CapsuleError>() {
        return from_capsule_error(core_err, causes);
    }

    let message = err.to_string();
    if message.contains(error_codes::ATO_ERR_AUTH_REQUIRED) {
        return CliDiagnostic::new(
            CliDiagnosticCode::E201,
            message,
            Some("`ato login`、`ato login --headless`、または `ATO_TOKEN=<token>` を使って再試行してください。"),
            None,
            None,
            causes,
        );
    }
    if message.contains(error_codes::ATO_ERR_INTEGRITY_FAILURE) {
        return CliDiagnostic::new(
            CliDiagnosticCode::E999,
            message,
            Some("レジストリ整合性またはネットワーク改ざんの可能性があります。再試行しても継続する場合は管理者へ連絡してください。"),
            None,
            None,
            causes,
        );
    }
    if message.contains("ingress TLS bootstrap required") {
        return CliDiagnostic::new(
            CliDiagnosticCode::E999,
            message,
            Some("`ato binding bootstrap-tls --binding <binding-id> [--install-system-trust]` を実行して明示的に TLS をセットアップしてください。"),
            None,
            None,
            causes,
        );
    }
    if message.contains("ingress TLS bootstrap requires explicit consent")
        || message.contains("ingress TLS trust installation failed")
    {
        return CliDiagnostic::new(
            CliDiagnosticCode::E999,
            message,
            Some("TLS trust bootstrap は明示的同意が必要です。内容を確認して `ato binding bootstrap-tls --binding <binding-id> --install-system-trust --yes` を再実行してください。"),
            None,
            None,
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
        causes,
    )
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
        CliDiagnosticCode::E999 => error_codes::EXIT_SYSTEM_ERROR,
        _ => error_codes::EXIT_USER_ERROR,
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
                    causes,
                );
            }
            CliDiagnostic::new(
                CliDiagnosticCode::E002,
                detail,
                Some("schema_version=0.2 と Manifest スキーマの整合性を確認してください。"),
                Some(path.as_path()),
                detect_field(detail),
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
                    causes,
                );
            }
            CliDiagnostic::new(
                CliDiagnosticCode::E102,
                detail,
                Some("build 設定・依存関係を確認し、必要に応じてコマンドを再実行してください。"),
                None,
                None,
                causes,
            )
        }
        capsule_core::CapsuleError::StrictManifestFallbackNotAllowed(detail) => CliDiagnostic::new(
            CliDiagnosticCode::E102,
            detail,
            Some(
                "--strict-v3 を無効化するか、source_digest をCASに登録して manifest 経路を成功させてください。",
            ),
            None,
            Some("strict-v3"),
            causes,
        ),
        capsule_core::CapsuleError::AuthRequired(detail) => CliDiagnostic::new(
            CliDiagnosticCode::E201,
            format!("Authentication required: {}", detail),
            Some("`ato login` を実行して認証情報を設定してください。"),
            None,
            None,
            causes,
        ),
        other => CliDiagnostic::new(
            CliDiagnosticCode::E999,
            other.to_string(),
            Some("再実行時に `RUST_BACKTRACE=1` を付けて詳細ログを確認してください。"),
            None,
            None,
            causes,
        ),
    }
}

fn collect_causes(err: &AnyhowError) -> Vec<String> {
    let mut values: Vec<String> = Vec::new();
    for cause in err.chain().skip(1) {
        let value = cause.to_string();
        if values.last() != Some(&value) {
            values.push(value);
        }
    }
    values
}

fn is_manifest_parse(message: &str) -> bool {
    message.contains("Failed to parse manifest TOML")
        || message.contains("TOML parse error")
        || message.contains("expected")
            && message.contains("capsule.toml")
            && message.to_ascii_lowercase().contains("parse")
}

fn is_required_field_issue(message: &str) -> bool {
    message.contains("default_target is required")
        || message.contains("Missing required field")
        || message.contains("Missing required [targets] table")
        || message.contains("At least one [targets.<label>] entry is required")
        || message.contains("default_target") && message.contains("does not exist under [targets]")
}

fn is_entrypoint_issue(message: &str) -> bool {
    message.contains("Entrypoint not found")
        || message.contains("No entrypoint defined in capsule.toml")
        || message.contains("entrypoint")
            && (message.contains("does not exist") || message.contains("Path:"))
}

fn is_source_registration_issue(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    message.contains("Source registration")
        || message.contains("GitHub")
        || message.contains("authentication")
        || lower.contains("register source")
        || lower.contains("source register")
}

fn is_publish_version_exists_conflict(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    (lower.contains("artifact upload") && lower.contains("(409"))
        && (lower.contains("version_exists")
            || lower.contains("same version is already published")
            || lower.contains("sha256 mismatch"))
}

fn is_manual_intervention_issue(message: &str) -> bool {
    message
        .to_ascii_lowercase()
        .contains("manual intervention required")
}

fn detect_field(message: &str) -> Option<&'static str> {
    if message.contains("default_target") {
        Some("default_target")
    } else if message.contains("[targets") || message.contains("targets.") {
        Some("targets")
    } else if message.contains("schema_version") {
        Some("schema_version")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use anyhow::anyhow;

    use super::{from_anyhow, CliDiagnosticCode, CommandContext, JsonErrorEnvelopeV1};

    #[test]
    fn maps_manifest_parse_to_e001() {
        let err = anyhow!(capsule_core::CapsuleError::Manifest(
            PathBuf::from("capsule.toml"),
            "Failed to parse manifest TOML: expected value".to_string()
        ));
        let diagnostic = from_anyhow(&err, CommandContext::Build);
        assert_eq!(diagnostic.code, CliDiagnosticCode::E001);
        assert_eq!(diagnostic.path.as_deref(), Some("capsule.toml"));
    }

    #[test]
    fn maps_required_default_target_to_e003() {
        let err = anyhow!(capsule_core::CapsuleError::Manifest(
            PathBuf::from("capsule.toml"),
            "Manifest validation failed: default_target is required".to_string()
        ));
        let diagnostic = from_anyhow(&err, CommandContext::Build);
        assert_eq!(diagnostic.code, CliDiagnosticCode::E003);
        assert_eq!(diagnostic.field.as_deref(), Some("default_target"));
    }

    #[test]
    fn maps_entrypoint_failure_to_e101() {
        let err = anyhow!(capsule_core::CapsuleError::Pack(
            "Entrypoint not found".to_string()
        ));
        let diagnostic = from_anyhow(&err, CommandContext::Build);
        assert_eq!(diagnostic.code, CliDiagnosticCode::E101);
    }

    #[test]
    fn maps_strict_manifest_error_to_e102() {
        let err = anyhow!(
            capsule_core::CapsuleError::StrictManifestFallbackNotAllowed(
                "fallback blocked".to_string()
            )
        );
        let diagnostic = from_anyhow(&err, CommandContext::Build);
        assert_eq!(diagnostic.code, CliDiagnosticCode::E102);
        assert_eq!(diagnostic.field.as_deref(), Some("strict-v3"));
        assert!(diagnostic
            .hint
            .as_deref()
            .unwrap_or_default()
            .contains("--strict-v3"));
    }

    #[test]
    fn maps_publish_version_exists_from_error_type_to_e202() {
        let err = anyhow!(
            crate::publish_artifact::PublishArtifactError::VersionExists {
                message: "same version is already published".to_string(),
            }
        );
        let diagnostic = from_anyhow(&err, CommandContext::Publish);
        assert_eq!(diagnostic.code, CliDiagnosticCode::E202);
    }

    #[test]
    fn maps_publish_version_exists_from_409_message_to_e202() {
        let err =
            anyhow!("Artifact upload failed (409 Conflict): same version is already published");
        let diagnostic = from_anyhow(&err, CommandContext::Publish);
        assert_eq!(diagnostic.code, CliDiagnosticCode::E202);
    }

    #[test]
    fn maps_manual_intervention_messages_to_e102() {
        let err = anyhow!(
            "manual intervention required: DATABASE_URL is required\nGenerated capsule.toml: /repo/.tmp/capsule.toml"
        );
        let diagnostic = from_anyhow(&err, CommandContext::Run);
        assert_eq!(diagnostic.code, CliDiagnosticCode::E102);
    }

    #[test]
    fn json_envelope_has_schema_version_1() {
        let err = anyhow!("untyped error");
        let diagnostic = from_anyhow(&err, CommandContext::Other);
        let envelope: JsonErrorEnvelopeV1 = diagnostic.to_json_envelope();
        assert_eq!(envelope.schema_version, "1");
        assert_eq!(envelope.error_type, "error");
    }

    #[test]
    fn maps_ingress_tls_bootstrap_required_to_actionable_hint() {
        let err = anyhow!(
            "ingress TLS bootstrap required for binding 'binding-demo'. Run `ato binding bootstrap-tls --binding binding-demo` first."
        );
        let diagnostic = from_anyhow(&err, CommandContext::Run);
        assert_eq!(diagnostic.code, CliDiagnosticCode::E999);
        assert!(diagnostic
            .hint
            .as_deref()
            .unwrap_or_default()
            .contains("bootstrap-tls"));
    }
}
