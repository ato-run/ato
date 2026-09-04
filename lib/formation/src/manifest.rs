//! `capsule.toml` → authored overrides. The one place a manifest becomes intent.
//!
//! ## Why this is here and nowhere else
//!
//! `FormationJobV1` has carried `authoring.manifest_toml` since the contract
//! was written and, until this module, nothing read it: the control plane
//! stored an uploaded `capsule.toml` verbatim, handed it to the worker, and
//! the worker dropped it on the floor. An author who wrote down how to launch
//! their App got a Static site built out of it instead.
//!
//! The fix has exactly one rule attached: the TOML is read HERE. Not in the
//! control plane, not in the PWA. A second reader is a second meaning, and the
//! first time the two disagree the disagreement shows up as an App that runs
//! something its author did not write.
//!
//! ## What this is not
//!
//! Not a Capsule manifest grammar. It reads the tables that change what
//! Formation does — `[tools]`, `[run]`, `[web]`, `[state.*]`, `[env]` — and
//! refuses, by name, every other table that WOULD change what Formation does
//! if it were honored. Silently ignoring `[build]` would produce a build that
//! skipped the author's build steps and reported success.
//!
//! Tables with no Formation meaning at all (`[metadata]`, `[source]`, and the
//! `schema_version` / `name` / `version` scalars) are ignored on purpose:
//! `[source]` selects which files are packed, and by the time Formation has a
//! tree that selection has already happened.
//!
//! ## Output vocabulary
//!
//! `AuthoredOverrides` — the same flat string map a job's
//! `authoring.overrides` carries, and the same one an App Preset expands into.
//! A manifest is therefore not a third way to describe an App; it is a
//! document that says the same things, and the compiler downstream cannot tell
//! which door an override came through.

use std::collections::BTreeMap;
use std::path::Path;

use toml::Value;

use crate::intent::AuthoredOverrides;

/// The file an author writes, at the root of the source they upload.
pub const MANIFEST_FILE_NAME: &str = "capsule.toml";

/// The only address a workload may bind for the Runner to reach it.
///
/// A process listening on loopback inside its sandbox is unreachable from the
/// outside, and the failure arrives as a readiness timeout that says nothing
/// about the cause.
const REQUIRED_BIND: &str = "0.0.0.0";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ManifestError {
    #[error("capsule.toml is not valid TOML: {detail}")]
    Syntax { detail: String },
    #[error("capsule.toml must be a table of settings")]
    NotATable,
    #[error(
        "capsule.toml declares [{section}], which this build does not honor; \
         remove it rather than have it silently ignored"
    )]
    UnsupportedSection { section: String },
    #[error("capsule.toml: {field} is malformed: {detail}")]
    Malformed { field: String, detail: String },
}

impl ManifestError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Syntax { .. } => "manifest_syntax",
            Self::NotATable => "manifest_not_a_table",
            Self::UnsupportedSection { .. } => "manifest_unsupported_section",
            Self::Malformed { .. } => "manifest_malformed",
        }
    }
}

fn malformed(field: impl Into<String>, detail: impl Into<String>) -> ManifestError {
    ManifestError::Malformed {
        field: field.into(),
        detail: detail.into(),
    }
}

/// Top-level keys that mean nothing to Formation, and are ignored rather than
/// refused. Each one is listed because it was CHECKED, not because it was
/// unrecognised: an unlisted key is refused so a new section cannot arrive as
/// silence.
const IGNORED_TOP_LEVEL: &[&str] = &[
    "schema_version",
    "name",
    "version",
    // Store copy. Never a build input.
    "metadata",
    // Which files are packed. Already applied by the time a tree exists here.
    "source",
];

/// Read the manifest sitting at the root of a source tree, if there is one.
///
/// `Ok(None)` is "this author wrote no manifest", which is the ordinary case
/// for a folder of HTML and stays a Static App. It is never "there was one and
/// it did not parse" — that is an error, because an author who wrote a
/// manifest and got a different App than they described has been ignored.
pub fn read_manifest_overrides(
    source_root: &Path,
) -> Result<Option<AuthoredOverrides>, ManifestError> {
    let path = source_root.join(MANIFEST_FILE_NAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ManifestError::Syntax {
                detail: format!("cannot read {MANIFEST_FILE_NAME}: {error}"),
            });
        }
    };
    parse_manifest_overrides(&text).map(Some)
}

/// Compile a manifest into the override vocabulary the intent compiler reads.
pub fn parse_manifest_overrides(text: &str) -> Result<AuthoredOverrides, ManifestError> {
    let parsed: Value = text.parse().map_err(|error| ManifestError::Syntax {
        detail: format!("{error}"),
    })?;
    let table = parsed.as_table().ok_or(ManifestError::NotATable)?;

    let mut overrides: BTreeMap<String, String> = BTreeMap::new();

    for (key, value) in table {
        match key.as_str() {
            "tools" => read_tools(value, &mut overrides)?,
            "run" => read_run(value, &mut overrides)?,
            "web" => read_web(value, &mut overrides)?,
            "state" => read_state(value, &mut overrides)?,
            "env" => read_env(value, &mut overrides)?,
            other if IGNORED_TOP_LEVEL.contains(&other) => {}
            other => {
                return Err(ManifestError::UnsupportedSection {
                    section: other.to_owned(),
                });
            }
        }
    }

    Ok(AuthoredOverrides(overrides))
}

/// `[tools]` — the exact runtime versions the author pinned.
///
/// Only `python` has a meaning downstream. Another tool name is refused rather
/// than dropped: a person who pinned a version and got a different one has
/// been overruled without being told, and the mismatch surfaces at import time.
fn read_tools(
    value: &Value,
    overrides: &mut BTreeMap<String, String>,
) -> Result<(), ManifestError> {
    let table = value
        .as_table()
        .ok_or_else(|| malformed("[tools]", "expected a table"))?;
    for (name, declared) in table {
        let version = declared
            .as_str()
            .ok_or_else(|| malformed(format!("tools.{name}"), "expected a version string"))?;
        match name.as_str() {
            "python" => {
                overrides.insert("runtime.python".to_owned(), version.to_owned());
            }
            other => {
                return Err(malformed(
                    format!("tools.{other}"),
                    "this build resolves only `python` from [tools]",
                ));
            }
        }
    }
    Ok(())
}

/// `[run]` — the launch, exactly as authored.
///
/// A declared launch also declares the lane, because a process is the only
/// thing a launch can be and this build has exactly one process lane. That is
/// not inference from a framework or a filename: it is what the author wrote.
fn read_run(value: &Value, overrides: &mut BTreeMap<String, String>) -> Result<(), ManifestError> {
    let table = value
        .as_table()
        .ok_or_else(|| malformed("[run]", "expected a table"))?;
    for key in table.keys() {
        if key != "command" {
            return Err(malformed(
                format!("run.{key}"),
                "[run] carries a `command` and nothing else",
            ));
        }
    }
    let command = table
        .get("command")
        .ok_or_else(|| malformed("run.command", "a launch is declared, never guessed"))?;
    let argv = match command {
        Value::String(line) => line.trim().to_owned(),
        // The array form, which is what a full Capsule manifest carries. Joined
        // rather than re-parsed, so both spellings reach the intent compiler as
        // one string and one splitter.
        Value::Array(items) => {
            let mut parts = Vec::with_capacity(items.len());
            for item in items {
                let word = item.as_str().ok_or_else(|| {
                    malformed("run.command", "every argv element must be a string")
                })?;
                if word.contains('"') || word.contains('\'') {
                    return Err(malformed(
                        "run.command",
                        format!(
                            "argv element {word:?} contains a quote, which this form cannot carry"
                        ),
                    ));
                }
                parts.push(if word.contains(char::is_whitespace) {
                    format!("\"{word}\"")
                } else {
                    word.to_owned()
                });
            }
            parts.join(" ")
        }
        _ => {
            return Err(malformed(
                "run.command",
                "expected a command line or an argv array",
            ));
        }
    };
    if argv.is_empty() {
        return Err(malformed("run.command", "is empty"));
    }
    overrides.insert("launch.argv".to_owned(), argv);
    overrides.insert("lane".to_owned(), "python_process".to_owned());
    Ok(())
}

/// `[web]` — the port the workload listens on, and how to tell it is up.
fn read_web(value: &Value, overrides: &mut BTreeMap<String, String>) -> Result<(), ManifestError> {
    let table = value
        .as_table()
        .ok_or_else(|| malformed("[web]", "expected a table"))?;
    for (key, declared) in table {
        match key.as_str() {
            "port" => {
                let port = declared
                    .as_integer()
                    .filter(|port| (1..=65_535).contains(port))
                    .ok_or_else(|| malformed("web.port", "expected a port between 1 and 65535"))?;
                overrides.insert("port.http".to_owned(), port.to_string());
            }
            "readiness_path" => {
                let path = declared
                    .as_str()
                    .filter(|path| path.starts_with('/'))
                    .ok_or_else(|| {
                        malformed("web.readiness_path", "expected an absolute request path")
                    })?;
                overrides.insert("readiness.http_path".to_owned(), path.to_owned());
            }
            // Accepted so a full Capsule manifest is readable here, and checked
            // rather than ignored: a workload bound to loopback cannot be
            // reached from outside its sandbox, and the failure arrives as a
            // readiness timeout that says nothing about why.
            "bind" => {
                let bind = declared
                    .as_str()
                    .ok_or_else(|| malformed("web.bind", "expected an address"))?;
                if bind != REQUIRED_BIND {
                    return Err(malformed(
                        "web.bind",
                        format!(
                            "must be {REQUIRED_BIND}; {bind:?} is unreachable from outside the sandbox"
                        ),
                    ));
                }
            }
            other => {
                return Err(malformed(
                    format!("web.{other}"),
                    "[web] carries `port`, `readiness_path` and `bind`",
                ));
            }
        }
    }
    Ok(())
}

/// `[state.<key>]` — a slot exists because this said so, and for no other
/// reason.
///
/// There is no "it looks like it writes to /data". A slot nobody declared is a
/// directory whose contents vanish at the end of the Run, which reads as data
/// loss to the person whose data it was.
fn read_state(
    value: &Value,
    overrides: &mut BTreeMap<String, String>,
) -> Result<(), ManifestError> {
    let table = value
        .as_table()
        .ok_or_else(|| malformed("[state]", "expected a table of slots"))?;
    for (state_key, slot) in table {
        if state_key.is_empty()
            || !state_key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(malformed(
                format!("state.{state_key}"),
                "a state key is letters, digits, `_` and `-`",
            ));
        }
        let slot = slot
            .as_table()
            .ok_or_else(|| malformed(format!("state.{state_key}"), "expected a table"))?;
        for (key, declared) in slot {
            match key.as_str() {
                "mount" => {
                    // Shape is checked downstream, where every override —
                    // authored here or written by hand into a job — meets the
                    // same rule. Two mount checks would be two rules.
                    let mount = declared.as_str().ok_or_else(|| {
                        malformed(format!("state.{state_key}.mount"), "expected a guest path")
                    })?;
                    overrides.insert(format!("state.{state_key}.mount"), mount.to_owned());
                }
                "access" => {
                    let access = declared.as_str().unwrap_or_default();
                    if !matches!(access, "read-write" | "read_write") {
                        return Err(malformed(
                            format!("state.{state_key}.access"),
                            format!("{access:?} is not supported; a state slot is read-write"),
                        ));
                    }
                }
                other => {
                    return Err(malformed(
                        format!("state.{state_key}.{other}"),
                        "a state slot carries `mount` and `access`",
                    ));
                }
            }
        }
        if !overrides.contains_key(&format!("state.{state_key}.mount")) {
            return Err(malformed(
                format!("state.{state_key}"),
                "declares no `mount`, so there is nowhere for it to appear",
            ));
        }
    }
    Ok(())
}

/// `[env]` — values the workload sees, and that anyone who can read the build
/// can read too. A secret never travels this way; Formation refuses the word.
fn read_env(value: &Value, overrides: &mut BTreeMap<String, String>) -> Result<(), ManifestError> {
    let table = value
        .as_table()
        .ok_or_else(|| malformed("[env]", "expected a table"))?;
    for (name, declared) in table {
        let text = declared
            .as_str()
            .ok_or_else(|| malformed(format!("env.{name}"), "expected a string"))?;
        overrides.insert(format!("env.{name}"), text.to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
schema_version = "1"
name = "fastapi-sqlite-personal"

[tools]
python = "3.12.7"

[run]
command = "/opt/ato/toolchains/python/3.12.7/bin/python3 -m uvicorn main:app --host 0.0.0.0 --port 8000"

[web]
port = 8000
readiness_path = "/health"

[state.app_data]
mount = "/data"

[env]
APP_DB_PATH = "/data/app.sqlite"
"#;

    fn overrides_of(text: &str) -> BTreeMap<String, String> {
        parse_manifest_overrides(text).expect("fixture parses").0
    }

    #[test]
    fn the_step_10_fixture_compiles_to_the_override_vocabulary() {
        let overrides = overrides_of(FIXTURE);
        assert_eq!(
            overrides.get("lane").map(String::as_str),
            Some("python_process")
        );
        assert_eq!(
            overrides.get("runtime.python").map(String::as_str),
            Some("3.12.7")
        );
        assert_eq!(
            overrides.get("launch.argv").map(String::as_str),
            Some(
                "/opt/ato/toolchains/python/3.12.7/bin/python3 -m uvicorn main:app --host 0.0.0.0 --port 8000"
            )
        );
        assert_eq!(overrides.get("port.http").map(String::as_str), Some("8000"));
        assert_eq!(
            overrides.get("readiness.http_path").map(String::as_str),
            Some("/health")
        );
        assert_eq!(
            overrides.get("state.app_data.mount").map(String::as_str),
            Some("/data")
        );
        assert_eq!(
            overrides.get("env.APP_DB_PATH").map(String::as_str),
            Some("/data/app.sqlite")
        );
    }

    #[test]
    fn a_manifest_with_no_state_table_declares_no_slot() {
        let overrides = overrides_of(
            r#"
[run]
command = "python3 app.py"
[web]
port = 8000
"#,
        );
        assert!(
            overrides.keys().all(|key| !key.starts_with("state.")),
            "a slot must never be invented: {overrides:?}"
        );
    }

    #[test]
    fn a_manifest_with_no_run_table_declares_no_launch() {
        let overrides = overrides_of("schema_version = \"1\"\nname = \"site\"\n");
        assert!(overrides.is_empty(), "{overrides:?}");
    }

    #[test]
    fn an_argv_array_reaches_the_same_launch_string() {
        let overrides = overrides_of(
            r#"
[run]
command = ["python3", "-m", "uvicorn", "main:app"]
"#,
        );
        assert_eq!(
            overrides.get("launch.argv").map(String::as_str),
            Some("python3 -m uvicorn main:app")
        );
    }

    #[test]
    fn an_argv_element_with_a_space_is_quoted_back() {
        let overrides = overrides_of("[run]\ncommand = [\"python3\", \"--note\", \"two words\"]\n");
        assert_eq!(
            overrides.get("launch.argv").map(String::as_str),
            Some("python3 --note \"two words\"")
        );
    }

    #[test]
    fn a_run_table_without_a_command_is_refused() {
        let error = parse_manifest_overrides("[run]\n").unwrap_err();
        assert_eq!(error.code(), "manifest_malformed");
        assert!(format!("{error}").contains("never guessed"), "{error}");
    }

    #[test]
    fn a_section_that_would_change_the_build_is_refused_by_name() {
        for section in ["build", "config", "outputs", "surface"] {
            let error = parse_manifest_overrides(&format!("[{section}]\n")).unwrap_err();
            assert_eq!(
                error,
                ManifestError::UnsupportedSection {
                    section: section.to_owned()
                },
                "[{section}] must be refused rather than ignored"
            );
        }
    }

    #[test]
    fn metadata_and_source_are_ignored_rather_than_refused() {
        let overrides = overrides_of(
            r#"
schema_version = "1"
name = "x"
version = "0.1.0"
[metadata]
tags = ["demo"]
[source]
root = "."
"#,
        );
        assert!(overrides.is_empty(), "{overrides:?}");
    }

    #[test]
    fn a_tool_this_build_cannot_provision_is_refused_rather_than_dropped() {
        let error = parse_manifest_overrides("[tools]\nruby = \"3.3\"\n").unwrap_err();
        assert_eq!(error.code(), "manifest_malformed");
        assert!(format!("{error}").contains("tools.ruby"), "{error}");
    }

    #[test]
    fn a_read_only_state_slot_is_refused() {
        let error = parse_manifest_overrides(
            "[state.app_data]\nmount = \"/data\"\naccess = \"read-only\"\n",
        )
        .unwrap_err();
        assert_eq!(error.code(), "manifest_malformed");
        assert!(format!("{error}").contains("read-write"), "{error}");
    }

    #[test]
    fn a_state_slot_without_a_mount_is_refused() {
        let error =
            parse_manifest_overrides("[state.app_data]\naccess = \"read-write\"\n").unwrap_err();
        assert!(format!("{error}").contains("no `mount`"), "{error}");
    }

    #[test]
    fn a_state_key_that_would_collide_with_the_override_vocabulary_is_refused() {
        let error =
            parse_manifest_overrides("[state.\"a.mount\"]\nmount = \"/data\"\n").unwrap_err();
        assert_eq!(error.code(), "manifest_malformed");
    }

    #[test]
    fn a_loopback_bind_is_refused_rather_than_timing_out_later() {
        let error =
            parse_manifest_overrides("[web]\nport = 8000\nbind = \"127.0.0.1\"\n").unwrap_err();
        assert!(format!("{error}").contains("unreachable"), "{error}");
    }

    #[test]
    fn a_port_outside_the_range_is_refused() {
        let error = parse_manifest_overrides("[web]\nport = 0\n").unwrap_err();
        assert_eq!(error.code(), "manifest_malformed");
    }

    #[test]
    fn broken_toml_is_a_syntax_error_and_not_an_empty_manifest() {
        let error = parse_manifest_overrides("[run\n").unwrap_err();
        assert_eq!(error.code(), "manifest_syntax");
    }

    #[test]
    fn a_source_tree_without_a_manifest_reads_as_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_manifest_overrides(dir.path()), Ok(None));
    }

    #[test]
    fn a_source_tree_with_a_manifest_reads_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(MANIFEST_FILE_NAME), FIXTURE).expect("write");
        let overrides = read_manifest_overrides(dir.path())
            .expect("reads")
            .expect("present");
        assert_eq!(overrides.get("port.http"), Some("8000"));
    }
}
