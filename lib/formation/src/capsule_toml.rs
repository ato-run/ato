//! `capsule.toml` → `AuthoringDraft`. One of two frontends; the other is a
//! Preset.
//!
//! ## What this file is for
//!
//! `capsule.toml` is an authoring format for a **Contract** — the observable
//! conditions that decide what counts as the same resumable point — and a
//! **Derivation** that proposes one way to reach it. It is not a software
//! manifest, not a build description, not a lockfile, and not the Capsule's
//! identity bytes. It is a route proposal plus, when a new Capsule is being
//! formed, the identity that route is trying to satisfy.
//!
//! ## Grammar ownership
//!
//! This parser owns exactly one document type: `schema = "ato.capsule/1"`.
//!
//! The legacy store Unified Manifest — `schema_version = "1"`, with its
//! `build` / `run` / `outputs` / `surface` tables — is a different grammar with
//! a different owner, and is never fed to this parser. Deciding by shape rather
//! than by declared schema is what produced the last regression: a store
//! manifest that happened to sit in a folder was read as though it described a
//! route, and refused a build that had worked.
//!
//! A document that declares neither schema is refused by name. It is never
//! quietly handed to a Preset — an author who supplied a route and a contract
//! must not have them silently replaced by a guess.
//!
//! ## Strict, and strict all the way down
//!
//! Unknown fields are refused at every level, not only the top. A key nobody
//! reads is a decision the author believes they made.

use std::collections::BTreeMap;

use toml::Value;

use crate::authoring::{
    AuthoringDraft, AuthoringError, AuthoringProvenance, BROWSER_PROTOCOL, ContractDraft,
    DerivationDraft, EffectClass, HTTP_PROTOCOL, HttpRequirement, InputDraft,
    InputIdentityRequirement, ObservationDraft, Observed, PROCESS_PROTOCOL, PortDraft,
    RequirementDraft, RuntimeDraft, STATE_FILESYSTEM_PROTOCOL, StateAccess, StateDraft, StepDraft,
    WORKSPACE_PROTOCOL, malformed,
};

/// The file an author writes, at the root of the source they upload.
pub const CAPSULE_FILE_NAME: &str = "capsule.toml";

/// This parser's document type.
pub const CAPSULE_SCHEMA_V1: &str = "ato.capsule/1";

/// The legacy store manifest's discriminator. Recognised here ONLY so it can be
/// handed back to its own owner by name instead of being misread.
pub const LEGACY_STORE_SCHEMA_KEY: &str = "schema_version";

/// The contract verifiers an author may name, spelled as the `use` key.
const USE_HTTP_CONTRACT: &str = "ato.contract.http@1";
const USE_WORKSPACE_CONTRACT: &str = "ato.contract.workspace@1";

/// The only address a workload may bind for anything outside to reach it.
const REQUIRED_BIND: &str = "0.0.0.0";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CapsuleTomlError {
    #[error("capsule.toml is not valid TOML: {detail}")]
    Syntax { detail: String },
    #[error("capsule.toml declares no `schema`; this build reads {CAPSULE_SCHEMA_V1} documents")]
    NoSchema,
    #[error(
        "capsule.toml declares schema {found:?}, which this build does not read; \
         it reads {CAPSULE_SCHEMA_V1}"
    )]
    UnsupportedSchema { found: String },
    /// The legacy store manifest, recognised and declined rather than misread.
    #[error(
        "capsule.toml is a store submission manifest (schema_version), which is a \
         different grammar with a different owner and is not a Capsule route"
    )]
    LegacyStoreManifest,
    #[error("{0}")]
    Authoring(#[from] AuthoringError),
}

impl CapsuleTomlError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Syntax { .. } => "capsule_toml_syntax",
            Self::NoSchema | Self::UnsupportedSchema { .. } => "unsupported_capsule_schema",
            Self::LegacyStoreManifest => "legacy_store_manifest",
            Self::Authoring(error) => error.code(),
        }
    }
}

/// Which grammar a `capsule.toml` belongs to, decided by what it declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapsuleDocumentKind {
    /// `schema = "ato.capsule/1"` — this parser's.
    CapsuleV1,
    /// `schema_version = "1"` — the store submission manifest's.
    LegacyStoreManifest,
    /// Neither. Refused; never guessed at.
    Unknown,
}

/// Read the discriminator, and nothing else.
///
/// Separated from parsing so a caller that only routes documents — the control
/// plane does — never has to load this grammar to know a document is not its.
pub fn classify(text: &str) -> Result<CapsuleDocumentKind, CapsuleTomlError> {
    let parsed: Value = text.parse().map_err(|error| CapsuleTomlError::Syntax {
        detail: format!("{error}"),
    })?;
    let Some(table) = parsed.as_table() else {
        return Ok(CapsuleDocumentKind::Unknown);
    };
    if let Some(schema) = table.get("schema").and_then(Value::as_str) {
        return Ok(if schema == CAPSULE_SCHEMA_V1 {
            CapsuleDocumentKind::CapsuleV1
        } else {
            CapsuleDocumentKind::Unknown
        });
    }
    if table.contains_key(LEGACY_STORE_SCHEMA_KEY) {
        return Ok(CapsuleDocumentKind::LegacyStoreManifest);
    }
    Ok(CapsuleDocumentKind::Unknown)
}

/// Read the file at a source root, if there is one.
///
/// `Ok(None)` means the author wrote no `capsule.toml`, which is the ordinary
/// case and the only case a Preset may synthesize for. It never means "there
/// was one and it did not parse".
pub fn read_capsule_toml(
    source_root: &std::path::Path,
) -> Result<Option<String>, CapsuleTomlError> {
    match std::fs::read_to_string(source_root.join(CAPSULE_FILE_NAME)) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(CapsuleTomlError::Syntax {
            detail: format!("cannot read {CAPSULE_FILE_NAME}: {error}"),
        }),
    }
}

/// Strict-parse an `ato.capsule/1` document into a draft.
pub fn parse_capsule_toml(text: &str) -> Result<AuthoringDraft, CapsuleTomlError> {
    match classify(text)? {
        CapsuleDocumentKind::CapsuleV1 => {}
        CapsuleDocumentKind::LegacyStoreManifest => {
            return Err(CapsuleTomlError::LegacyStoreManifest);
        }
        CapsuleDocumentKind::Unknown => {
            let parsed: Value = text.parse().map_err(|error| CapsuleTomlError::Syntax {
                detail: format!("{error}"),
            })?;
            return Err(match parsed.get("schema").and_then(Value::as_str) {
                Some(found) => CapsuleTomlError::UnsupportedSchema {
                    found: found.to_owned(),
                },
                None => CapsuleTomlError::NoSchema,
            });
        }
    }

    let parsed: Value = text.parse().map_err(|error| CapsuleTomlError::Syntax {
        detail: format!("{error}"),
    })?;
    let table = parsed.as_table().expect("classify accepted a table");

    let mut derivation = DerivationDraft::default();
    let mut contract = ContractDraft::default();

    for (key, value) in table {
        match key.as_str() {
            "schema" => {}
            "input" => derivation.inputs = read_inputs(value)?,
            "runtime" => derivation.runtimes = read_runtimes(value)?,
            "derive" => derivation.steps = read_derive(value)?,
            "port" => derivation.ports = read_ports(value)?,
            "state" => derivation.state = read_state(value)?,
            "contract" => contract.requirements = read_contract(value)?,
            "effects" => derivation.effects = read_effects(value)?,
            other => {
                return Err(malformed(
                    other,
                    "this build reads schema, input, runtime, derive, port, state, \
                     contract and effects",
                )
                .into());
            }
        }
    }

    Ok(AuthoringDraft {
        contract,
        derivation,
        provenance: AuthoringProvenance::Authored,
    })
}

// ─────────────────────────────────────────────────────────────────── the tables

fn array_of_tables<'a>(
    value: &'a Value,
    what: &str,
) -> Result<Vec<&'a toml::map::Map<String, Value>>, AuthoringError> {
    let items = value
        .as_array()
        .ok_or_else(|| malformed(what, format!("expected [[{what}]] entries")))?;
    items
        .iter()
        .map(|item| {
            item.as_table()
                .ok_or_else(|| malformed(what, "expected a table"))
        })
        .collect()
}

fn required_str(
    table: &toml::map::Map<String, Value>,
    field: &str,
    what: &str,
) -> Result<String, AuthoringError> {
    table
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| malformed(format!("{what}.{field}"), "is required"))
}

/// Refuse a key nobody reads.
fn only(
    table: &toml::map::Map<String, Value>,
    what: &str,
    allowed: &[&str],
) -> Result<(), AuthoringError> {
    for key in table.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(malformed(
                format!("{what}.{key}"),
                format!("is not read here; this table takes {}", allowed.join(", ")),
            ));
        }
    }
    Ok(())
}

fn read_inputs(value: &Value) -> Result<Vec<InputDraft>, AuthoringError> {
    array_of_tables(value, "input")?
        .into_iter()
        .map(|table| {
            only(table, "input", &["id", "use", "path"])?;
            let id = required_str(table, "id", "input")?;
            let protocol = required_str(table, "use", "input")?;
            if protocol != WORKSPACE_PROTOCOL {
                return Err(malformed(
                    format!("input.{id}.use"),
                    format!("{protocol:?} is not a protocol this build resolves; use {WORKSPACE_PROTOCOL}"),
                ));
            }
            Ok(InputDraft {
                id,
                protocol,
                path: table
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or(".")
                    .to_owned(),
            })
        })
        .collect()
}

fn read_runtimes(value: &Value) -> Result<Vec<RuntimeDraft>, AuthoringError> {
    array_of_tables(value, "runtime")?
        .into_iter()
        .map(|table| {
            only(table, "runtime", &["name", "version"])?;
            Ok(RuntimeDraft {
                name: required_str(table, "name", "runtime")?,
                version: required_str(table, "version", "runtime")?,
            })
        })
        .collect()
}

fn read_derive(value: &Value) -> Result<Vec<StepDraft>, AuthoringError> {
    let table = value
        .as_table()
        .ok_or_else(|| malformed("derive", "expected [[derive.step]] entries"))?;
    only(table, "derive", &["step"])?;
    let steps = table
        .get("step")
        .ok_or_else(|| malformed("derive", "declares no [[derive.step]]"))?;

    array_of_tables(steps, "derive.step")?
        .into_iter()
        .map(|step| {
            only(
                step,
                "derive.step",
                &[
                    "id",
                    "use",
                    "op",
                    "argv",
                    "cwd",
                    "env",
                    "source",
                    "root",
                    "entry",
                    "spa_fallback",
                ],
            )?;
            let id = required_str(step, "id", "derive.step")?;
            let protocol = required_str(step, "use", "derive.step")?;
            let op = required_str(step, "op", "derive.step")?;
            let what = format!("derive.step.{id}");

            let argv = match step.get("argv") {
                None => Vec::new(),
                Some(Value::Array(items)) => items
                    .iter()
                    .map(|item| {
                        item.as_str().map(str::to_owned).ok_or_else(|| {
                            malformed(format!("{what}.argv"), "every element must be a string")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                Some(_) => {
                    return Err(malformed(
                        format!("{what}.argv"),
                        "expected an array of strings — a launch is argv, never a command line \
                         somebody's shell has to interpret",
                    ));
                }
            };

            let mut env = BTreeMap::new();
            if let Some(declared) = step.get("env") {
                let declared = declared
                    .as_table()
                    .ok_or_else(|| malformed(format!("{what}.env"), "expected a table"))?;
                for (name, item) in declared {
                    let text = item.as_str().ok_or_else(|| {
                        malformed(format!("{what}.env.{name}"), "expected a string")
                    })?;
                    env.insert(name.clone(), text.to_owned());
                }
            }

            match protocol.as_str() {
                PROCESS_PROTOCOL => {
                    if !matches!(op.as_str(), "exec" | "serve") {
                        return Err(malformed(
                            format!("{what}.op"),
                            format!("{PROCESS_PROTOCOL} performs `exec` and `serve`"),
                        ));
                    }
                    if argv.is_empty() {
                        return Err(malformed(
                            format!("{what}.argv"),
                            "a launch is declared, never inferred from a framework or a filename",
                        ));
                    }
                }
                BROWSER_PROTOCOL => {
                    if op != "serve" {
                        return Err(malformed(
                            format!("{what}.op"),
                            format!("{BROWSER_PROTOCOL} performs `serve`"),
                        ));
                    }
                    if step.get("source").is_none() {
                        return Err(malformed(
                            format!("{what}.source"),
                            "names the input whose tree is served",
                        ));
                    }
                }
                other => {
                    return Err(malformed(
                        format!("{what}.use"),
                        format!(
                            "{other:?} is not a protocol this build executes; \
                             use {PROCESS_PROTOCOL} or {BROWSER_PROTOCOL}"
                        ),
                    ));
                }
            }

            Ok(StepDraft {
                id,
                protocol,
                op,
                argv,
                cwd: step
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                env,
                source: step
                    .get("source")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                root: step.get("root").and_then(Value::as_str).map(str::to_owned),
                entry: step.get("entry").and_then(Value::as_str).map(str::to_owned),
                spa_fallback: step.get("spa_fallback").and_then(Value::as_bool),
            })
        })
        .collect()
}

fn read_ports(value: &Value) -> Result<Vec<PortDraft>, AuthoringError> {
    array_of_tables(value, "port")?
        .into_iter()
        .map(|table| {
            only(table, "port", &["id", "use", "from", "guest_port", "bind"])?;
            let id = required_str(table, "id", "port")?;
            let protocol = required_str(table, "use", "port")?;
            if protocol != HTTP_PROTOCOL {
                return Err(malformed(
                    format!("port.{id}.use"),
                    format!("{protocol:?} is not a port protocol this build exports; use {HTTP_PROTOCOL}"),
                ));
            }
            // Checked rather than ignored: a workload bound to loopback cannot
            // be reached from outside its sandbox, and the failure arrives as a
            // readiness timeout that says nothing about why.
            if let Some(bind) = table.get("bind").and_then(Value::as_str)
                && bind != REQUIRED_BIND
            {
                return Err(malformed(
                    format!("port.{id}.bind"),
                    format!("must be {REQUIRED_BIND}; {bind:?} is unreachable from outside the sandbox"),
                ));
            }
            let guest_port = match table.get("guest_port") {
                None => None,
                Some(value) => Some(
                    value
                        .as_integer()
                        .filter(|port| (1..=65_535).contains(port))
                        .map(|port| port as u16)
                        .ok_or_else(|| {
                            malformed(
                                format!("port.{id}.guest_port"),
                                "expected a port between 1 and 65535",
                            )
                        })?,
                ),
            };
            Ok(PortDraft {
                id,
                protocol,
                from: required_str(table, "from", "port")?,
                guest_port,
            })
        })
        .collect()
}

fn read_state(value: &Value) -> Result<Vec<StateDraft>, AuthoringError> {
    array_of_tables(value, "state")?
        .into_iter()
        .map(|table| {
            only(table, "state", &["id", "use", "mount", "access"])?;
            let id = required_str(table, "id", "state")?;
            let protocol = required_str(table, "use", "state")?;
            if protocol != STATE_FILESYSTEM_PROTOCOL {
                return Err(malformed(
                    format!("state.{id}.use"),
                    format!("{protocol:?} is not a state protocol this build carries; use {STATE_FILESYSTEM_PROTOCOL}"),
                ));
            }
            let access = match table.get("access").and_then(Value::as_str) {
                None | Some("read-write") => StateAccess::ReadWrite,
                Some("read-only") => StateAccess::ReadOnly,
                Some(other) => {
                    return Err(malformed(
                        format!("state.{id}.access"),
                        format!("{other:?} is not an access mode; use read-write or read-only"),
                    ));
                }
            };
            Ok(StateDraft {
                id,
                protocol,
                mount: required_str(table, "mount", "state")?,
                access,
            })
        })
        .collect()
}

fn read_contract(value: &Value) -> Result<Vec<ObservationDraft>, AuthoringError> {
    let table = value
        .as_table()
        .ok_or_else(|| malformed("contract", "expected [[contract.require]] entries"))?;
    only(table, "contract", &["require"])?;
    let requires = table
        .get("require")
        .ok_or_else(|| malformed("contract", "declares no [[contract.require]]"))?;

    array_of_tables(requires, "contract.require")?
        .into_iter()
        .map(|require| {
            only(
                require,
                "contract.require",
                &["id", "use", "port", "method", "path", "input", "expect"],
            )?;
            let id = required_str(require, "id", "contract.require")?;
            let verifier = required_str(require, "use", "contract.require")?;
            let what = format!("contract.require.{id}");
            let expect = match require.get("expect") {
                None => &toml::map::Map::new() as &toml::map::Map<String, Value>,
                Some(value) => value
                    .as_table()
                    .ok_or_else(|| malformed(format!("{what}.expect"), "expected a table"))?,
            };

            let requirement = match verifier.as_str() {
                USE_HTTP_CONTRACT => {
                    only(
                        expect,
                        &format!("{what}.expect"),
                        &["status", "body_digest"],
                    )?;
                    let method = require
                        .get("method")
                        .and_then(Value::as_str)
                        .unwrap_or("GET")
                        .to_owned();
                    if method != "GET" {
                        return Err(malformed(
                            format!("{what}.method"),
                            "this build observes GET; another method would change the \
                             continuation it claims to be observing",
                        ));
                    }
                    let path = require
                        .get("path")
                        .and_then(Value::as_str)
                        .unwrap_or("/")
                        .to_owned();
                    if !path.starts_with('/') {
                        return Err(malformed(
                            format!("{what}.path"),
                            "expected an absolute request path",
                        ));
                    }
                    let status = expect
                        .get("status")
                        .and_then(Value::as_integer)
                        .filter(|status| (100..=599).contains(status))
                        .map(|status| status as u16)
                        .ok_or_else(|| {
                            malformed(
                                format!("{what}.expect.status"),
                                "an HTTP observation states the status it expects",
                            )
                        })?;
                    let body_digest = match expect.get("body_digest") {
                        None => None,
                        Some(Value::String(value)) if value == "capture" => Some(Observed::Capture),
                        Some(Value::String(value)) => Some(Observed::Stated(value.clone())),
                        Some(_) => {
                            return Err(malformed(
                                format!("{what}.expect.body_digest"),
                                "expected a digest, or \"capture\"",
                            ));
                        }
                    };
                    RequirementDraft::Http(HttpRequirement {
                        port: required_str(require, "port", &what)?,
                        method,
                        path,
                        status,
                        body_digest,
                    })
                }
                USE_WORKSPACE_CONTRACT => {
                    only(expect, &format!("{what}.expect"), &["digest"])?;
                    let digest = match expect.get("digest") {
                        Some(Value::String(value)) if value == "capture" => Observed::Capture,
                        Some(Value::String(value)) => Observed::Stated(value.clone()),
                        _ => {
                            return Err(malformed(
                                format!("{what}.expect.digest"),
                                "expected a digest, or \"capture\" to freeze the one observed \
                                 when this Capsule is sealed",
                            ));
                        }
                    };
                    RequirementDraft::InputIdentity(InputIdentityRequirement {
                        input: required_str(require, "input", &what)?,
                        digest,
                    })
                }
                other => {
                    return Err(malformed(
                        format!("{what}.use"),
                        format!(
                            "{other:?} is not a verifier this build can decide; \
                             use {USE_HTTP_CONTRACT} or {USE_WORKSPACE_CONTRACT}"
                        ),
                    ));
                }
            };
            Ok(ObservationDraft { id, requirement })
        })
        .collect()
}

fn read_effects(value: &Value) -> Result<EffectClass, AuthoringError> {
    let table = value
        .as_table()
        .ok_or_else(|| malformed("effects", "expected a table"))?;
    only(table, "effects", &["default"])?;
    match table.get("default").and_then(Value::as_str) {
        None => Ok(EffectClass::Pure),
        Some("pure") => Ok(EffectClass::Pure),
        Some("idempotent") => Ok(EffectClass::Idempotent),
        Some("record-substitutable") => Ok(EffectClass::RecordSubstitutable),
        Some("requires-confirmation") => Ok(EffectClass::RequiresConfirmation),
        Some("non-repeatable") => Ok(EffectClass::NonRepeatable),
        Some(other) => Err(malformed(
            "effects.default",
            format!(
                "{other:?} is not an effect class; use pure, idempotent, \
                 record-substitutable, requires-confirmation or non-repeatable"
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_store_submission_manifest_is_declined_by_name_not_misread() {
        // The regression that started this: a store manifest sitting in a
        // folder was read as though it described a route. It has its own
        // grammar and its own owner, and this parser says so rather than
        // guessing.
        let error = parse_capsule_toml(
            "schema_version = \"1\"\nname = \"x\"\nversion = \"1\"\n[run]\ncommand = [\"x\"]\n",
        )
        .unwrap_err();
        assert_eq!(error.code(), "legacy_store_manifest");
        assert_eq!(
            classify("schema_version = \"1\"\n").unwrap(),
            CapsuleDocumentKind::LegacyStoreManifest
        );
    }

    #[test]
    fn a_document_declaring_neither_schema_is_refused_rather_than_guessed_at() {
        for text in ["name = \"x\"\n", "schema = \"ato.capsule/2\"\n"] {
            let error = parse_capsule_toml(text).unwrap_err();
            assert_eq!(error.code(), "unsupported_capsule_schema", "{text}");
        }
    }

    #[test]
    fn an_unknown_field_is_refused_at_every_level() {
        for text in [
            // top level
            "schema = \"ato.capsule/1\"\nbuild = {}\n",
            // inside an array of tables
            "schema = \"ato.capsule/1\"\n[[input]]\nid=\"w\"\nuse=\"ato.workspace@1\"\nnope=1\n",
            // inside a nested expect table
            "schema = \"ato.capsule/1\"\n[[contract.require]]\nid=\"r\"\nuse=\"ato.contract.http@1\"\nport=\"p\"\n[contract.require.expect]\nstatus=200\nnope=1\n",
        ] {
            let error = parse_capsule_toml(text).unwrap_err();
            assert_eq!(error.code(), "authoring_malformed", "{text}");
        }
    }

    #[test]
    fn a_process_step_without_argv_is_refused_and_never_inferred() {
        let error = parse_capsule_toml(
            "schema = \"ato.capsule/1\"\n[[derive.step]]\nid=\"a\"\nuse=\"ato.process@1\"\nop=\"serve\"\n",
        )
        .unwrap_err();
        assert!(format!("{error}").contains("never inferred"), "{error}");
    }

    #[test]
    fn a_command_line_string_is_refused_in_favour_of_argv() {
        let error = parse_capsule_toml(
            "schema = \"ato.capsule/1\"\n[[derive.step]]\nid=\"a\"\nuse=\"ato.process@1\"\nop=\"serve\"\nargv=\"python3 -m x\"\n",
        )
        .unwrap_err();
        assert!(
            format!("{error}").contains("never a command line"),
            "{error}"
        );
    }

    #[test]
    fn an_invented_protocol_is_refused() {
        for text in [
            "schema = \"ato.capsule/1\"\n[[input]]\nid=\"w\"\nuse=\"ato.magic@1\"\n",
            "schema = \"ato.capsule/1\"\n[[derive.step]]\nid=\"a\"\nuse=\"ato.magic@1\"\nop=\"go\"\n",
            "schema = \"ato.capsule/1\"\n[[port]]\nid=\"p\"\nuse=\"ato.magic@1\"\nfrom=\"a\"\n",
            "schema = \"ato.capsule/1\"\n[[contract.require]]\nid=\"r\"\nuse=\"ato.magic@1\"\n",
        ] {
            assert_eq!(
                parse_capsule_toml(text).unwrap_err().code(),
                "authoring_malformed",
                "{text}"
            );
        }
    }

    #[test]
    fn an_effect_class_is_carried_and_never_invented() {
        let base = "schema = \"ato.capsule/1\"\n";
        assert_eq!(
            parse_capsule_toml(base).unwrap().derivation.effects,
            EffectClass::Pure,
            "a route that declares no external effect claims none"
        );
        let declared =
            parse_capsule_toml(&format!("{base}[effects]\ndefault = \"non-repeatable\"\n"))
                .unwrap();
        assert_eq!(declared.derivation.effects, EffectClass::NonRepeatable);
        assert!(parse_capsule_toml(&format!("{base}[effects]\ndefault = \"safe\"\n")).is_err());
    }

    #[test]
    fn parsing_records_that_a_person_wrote_this() {
        let draft = parse_capsule_toml("schema = \"ato.capsule/1\"\n").unwrap();
        assert_eq!(draft.provenance, AuthoringProvenance::Authored);
    }

    #[test]
    fn a_source_tree_without_a_capsule_toml_reads_as_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_capsule_toml(dir.path()), Ok(None));
    }
}
