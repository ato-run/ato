//! Collect Binding values for an admitted portable Run. Values are passed to
//! the runtime only, with no echo for confidential input.
use std::collections::{BTreeMap, btree_map::Entry};
use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ato_portable_application::{
    ApplicationBindingV1, OPENAI_EMBEDDING_API_KEY_PROTOCOL, OPENAI_EMBEDDING_BASE_URL_PROTOCOL,
    ValidatedApplication, resolve_application_bindings, validate_supplied_application_bindings,
};

pub(super) fn parse_env(value: &str) -> Result<(String, String), &'static str> {
    let (id, variable) = value
        .split_once('=')
        .ok_or("expected BINDING_ID=ENV_VAR, for example admin_password=ATO_ADMIN_PASSWORD")?;
    if id.is_empty()
        || variable.is_empty()
        || !variable.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphabetic() || byte == b'_' || index > 0 && byte.is_ascii_digit()
        })
    {
        return Err("expected a Binding ID and an environment variable name");
    }
    Ok((id.to_owned(), variable.to_owned()))
}

pub(super) fn resolve(
    application: &ValidatedApplication,
    inline: Vec<(String, String)>,
    env: Vec<(String, String)>,
    cancelled: &AtomicBool,
) -> Result<BTreeMap<String, String>> {
    let mut values = super::runtime_binding_values(inline)?;
    for (id, variable) in env {
        match values.entry(id) {
            Entry::Occupied(entry) => bail!(
                "runtime Binding `{}` was supplied more than once",
                entry.key()
            ),
            Entry::Vacant(entry) => {
                let value = std::env::var(&variable).with_context(|| {
                    format!(
                        "Set {variable} in the environment before using --bind-env {}={variable}",
                        entry.key()
                    )
                })?;
                entry.insert(value);
            }
        }
    }
    validate_supplied_application_bindings(application, &values)?;
    for binding in &application.bindings {
        if let Some(value) = values.get(&binding.id)
            && let Err(reason) = validate_value(binding, value)
        {
            bail!("Invalid value for `{}`. {reason}", binding.id);
        }
    }
    let missing = application
        .bindings
        .iter()
        .filter(|binding| binding.required && !values.contains_key(&binding.id))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() && !io::stdin().is_terminal() {
        // A Run without a terminal cannot ask for a value, so name every
        // required Binding and both ways to supply it before anything starts.
        bail!(
            "Required Bindings are missing. Supply them for this Run, then run the same command again:\n{}\nNo workload was started.",
            guidance(&missing, false).join("\n")
        );
    }

    if !missing.is_empty() {
        // The same list the non-interactive Run refuses with, so a person
        // knows every required name and how a script would pass it.
        eprintln!(
            "Required Bindings. Values apply to this Run only:\n{}",
            guidance(&missing, true).join("\n")
        );
    }

    for binding in &missing {
        let id = &binding.id;
        let (purpose, example) = describe(binding);
        let hidden_help = || {
            format!(
                "Could not read `{id}` without echo. Use --bind-env {id}=ATO_BIND_{}",
                id.to_ascii_uppercase()
            )
        };
        // Keep echo disabled while displaying an error and asking again.
        // A user can begin retyping before the next prompt finishes printing.
        let _echo = example
            .is_none()
            .then(TerminalEcho::disable)
            .transpose()
            .with_context(hidden_help)?;
        loop {
            if cancelled.load(Ordering::Relaxed) {
                bail!("the Run was cancelled. No workload was started");
            }
            let value = if let Some(example) = example {
                eprint!("{id} ({purpose}, for example {example}): ");
                io::stderr().flush()?;
                read_visible(cancelled)?.context("Input ended. No workload was started")?
            } else {
                let prompt = format!(
                    "{id} ({purpose}, input hidden. Use --bind-env for multiline values): "
                );
                match read_hidden(&prompt) {
                    Ok(value) => value,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::Interrupted | io::ErrorKind::UnexpectedEof
                        ) =>
                    {
                        bail!("the Run was cancelled. No workload was started")
                    }
                    Err(error) => return Err(error).with_context(hidden_help),
                }
            };
            match validate_value(binding, &value) {
                Ok(()) => {
                    values.insert(id.clone(), value);
                    break;
                }
                Err(reason) => eprintln!("Invalid value for `{id}`. {reason} Try again."),
            }
        }
    }
    resolve_application_bindings(application, &values).map_err(Into::into)
}

fn validate_value(binding: &ApplicationBindingV1, value: &str) -> Result<(), &'static str> {
    if value.is_empty() {
        return Err("A value is required.");
    }
    if value.len() > 64 * 1024 || value.contains('\0') {
        return Err("Enter at most 64 KiB without NUL characters.");
    }
    if binding.protocol == OPENAI_EMBEDDING_BASE_URL_PROTOCOL
        && !reqwest::Url::parse(value)
            .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.has_host())
    {
        return Err("Enter a complete HTTP or HTTPS URL.");
    }
    Ok(())
}

pub(super) fn read_visible(cancelled: &AtomicBool) -> Result<Option<String>> {
    // The portable Run handles Ctrl-C itself. Wait for either the line or
    // cancellation so a blocking terminal read does not swallow Ctrl-C.
    let (send, receive) = mpsc::channel();
    std::thread::Builder::new().spawn(move || {
        let mut value = String::new();
        let result = io::stdin()
            .read_line(&mut value)
            .map(|bytes| (bytes > 0).then(|| value.trim_end_matches(['\r', '\n']).to_owned()));
        let _ = send.send(result);
    })?;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            bail!("Input was cancelled");
        }
        match receive.recv_timeout(Duration::from_millis(50)) {
            Ok(value) => return Ok(value?),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("could not read the Binding value"),
        }
    }
}

/// One line per required Binding: what it is, and how to supply it.
///
/// A secret has no literal example and its prompt never echoes; `interactive`
/// decides whether the terminal prompt is one of the ways, because a Run
/// without a terminal cannot ask.
fn guidance(bindings: &[ApplicationBindingV1], interactive: bool) -> Vec<String> {
    bindings
        .iter()
        .filter(|binding| binding.required)
        .map(|binding| {
            let (purpose, example) = describe(binding);
            let variable = format!("ATO_BIND_{}", binding.id.to_ascii_uppercase());
            let env_flag = format!("--bind-env {}={variable}", binding.id);
            let id = &binding.id;
            match (example, interactive) {
                (Some(example), true) => format!(
                    "  {id} ({purpose}): type it when prompted, or --bind {id}=<value> (for example {example}) or {env_flag}"
                ),
                (Some(example), false) => format!(
                    "  {id} ({purpose}): --bind {id}=<value> (for example {example}) or {env_flag}"
                ),
                (None, true) => format!(
                    "  {id} ({purpose}): type it when prompted (input hidden), or {env_flag}"
                ),
                (None, false) => format!(
                    "  {id} ({purpose}): {env_flag} (read from the environment; there is no prompt without a terminal)"
                ),
            }
        })
        .collect()
}

/// Only documented, non-secret protocols get an echoed input and a literal
/// example. Unknown protocols may carry confidential values.
fn describe(binding: &ApplicationBindingV1) -> (&'static str, Option<&'static str>) {
    match binding.protocol.as_str() {
        "ato.config@1" => ("app setting", Some("example-value")),
        OPENAI_EMBEDDING_BASE_URL_PROTOCOL => ("service base URL", Some("https://api.example/v1")),
        "ato.secret@1" => ("secret such as a password or token", None),
        "ato.secret-refresh@1" => ("refresh credential", None),
        OPENAI_EMBEDDING_API_KEY_PROTOCOL => ("API key", None),
        ato_formation::authoring::TCP_EGRESS_PROTOCOL => ("outbound connection setting", None),
        _ => ("value requested by the app", None),
    }
}

fn read_hidden(prompt: &str) -> io::Result<String> {
    // The caller keeps echo off. This reader handles editing and Ctrl-C.
    let builder = rpassword::ConfigBuilder::new().output_writer(io::stderr());
    #[cfg(unix)]
    let builder = builder.input_file_path("/dev/fd/0");
    rpassword::prompt_password_with_config(prompt, builder.build())
}

#[cfg(unix)]
struct TerminalEcho(libc::termios);

#[cfg(unix)]
impl TerminalEcho {
    fn disable() -> io::Result<Self> {
        let mut original = std::mem::MaybeUninit::uninit();
        // SAFETY: tcgetattr initializes the termios at this address on success.
        let original = unsafe {
            if libc::tcgetattr(libc::STDIN_FILENO, original.as_mut_ptr()) != 0 {
                return Err(io::Error::last_os_error());
            }
            original.assume_init()
        };
        let mut hidden = original;
        hidden.c_lflag &= !(libc::ECHO | libc::ECHONL);
        // SAFETY: hidden is initialized and stdin is the terminal above.
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &hidden) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(original))
    }
}

#[cfg(unix)]
impl Drop for TerminalEcho {
    fn drop(&mut self) {
        // SAFETY: restore the termios originally read from stdin.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.0) };
    }
}

#[cfg(windows)]
struct TerminalEcho(
    windows::Win32::Foundation::HANDLE,
    windows::Win32::System::Console::CONSOLE_MODE,
);

#[cfg(windows)]
impl TerminalEcho {
    fn disable() -> io::Result<Self> {
        use windows::Win32::System::Console::{
            CONSOLE_MODE, ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE,
            SetConsoleMode,
        };
        let mut original = CONSOLE_MODE::default();
        // SAFETY: the borrowed standard handle is queried and updated with an
        // initialized console mode. This guard does not close the handle.
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE).map_err(io::Error::other)?;
            GetConsoleMode(handle, &mut original).map_err(io::Error::other)?;
            SetConsoleMode(handle, original & !ENABLE_ECHO_INPUT).map_err(io::Error::other)?;
            Ok(Self(handle, original))
        }
    }
}

#[cfg(windows)]
impl Drop for TerminalEcho {
    fn drop(&mut self) {
        // SAFETY: restore the mode on the same borrowed input handle.
        let _ = unsafe { windows::Win32::System::Console::SetConsoleMode(self.0, self.1) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(id: &str, protocol: &str) -> ApplicationBindingV1 {
        ApplicationBindingV1 {
            id: id.to_owned(),
            protocol: protocol.to_owned(),
            required: true,
        }
    }

    #[test]
    fn a_missing_required_binding_is_named_with_both_flags() {
        let lines = guidance(
            &[
                binding("admin_password", "ato.secret@1"),
                binding("site_name", "ato.config@1"),
            ],
            false,
        );
        assert_eq!(lines.len(), 2);
        // A secret has no literal example and is only ever asked for without
        // echo, so a non-interactive Run names the environment flag.
        assert!(lines[0].contains("admin_password"), "{lines:?}");
        assert!(
            lines[0].contains("--bind-env admin_password=ATO_BIND_ADMIN_PASSWORD"),
            "{lines:?}"
        );
        assert!(!lines[0].contains("--bind admin_password="), "{lines:?}");
        assert!(lines[1].contains("site_name"), "{lines:?}");
        assert!(lines[1].contains("--bind site_name=<value>"), "{lines:?}");
    }

    #[test]
    fn an_interactive_run_says_a_secret_is_prompted_without_echo() {
        let lines = guidance(&[binding("admin_password", "ato.secret@1")], true);
        assert!(
            lines[0].contains("type it when prompted (input hidden)"),
            "{lines:?}"
        );
        assert!(
            lines[0].contains("--bind-env admin_password=ATO_BIND_ADMIN_PASSWORD"),
            "{lines:?}"
        );
    }

    #[test]
    fn an_optional_binding_is_not_named_as_required() {
        let mut optional = binding("site_name", "ato.config@1");
        optional.required = false;
        assert!(guidance(&[optional], false).is_empty());
    }
}
