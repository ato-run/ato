//! ato-cert-export - validate an allowlisted certificate/key pair, export it,
//! and print the Binding update payload an operator-scoped updater sends.
//!
//! No network calls, no Run starts, no writes outside the export directory.

use std::path::PathBuf;
use std::process::ExitCode;

use ato_cert_export::allowlist;
use ato_cert_export::cert::OpensslCli;
use ato_cert_export::export::{self, Thresholds};
use ato_cert_export::{Error, Result, payload};

const DEFAULT_ALLOWLIST: &str = "/etc/ato/cert-export/allowlist.conf";
const DEFAULT_STATE_ROOT: &str = "/var/lib/ato/cert-export";
const DEFAULT_OPENSSL: &str = "/usr/bin/openssl";

const USAGE: &str = "\
ato-cert-export - certificate Binding exporter (ADR-044)

Usage:
  ato-cert-export check    --instance <ID> [options]
  ato-cert-export export   --instance <ID> [--print-binding-update] [options]
  ato-cert-export print-binding-update --instance <ID> --grant-id <ID> \
        --expected-generation <N> [options]

Commands:
  check                  Validate the allowlisted pair; write nothing.
  export                 Validate, then update the export directory. A pair whose
                         source bytes are unchanged is a no-op.
  print-binding-update   Print the Binding update payload for the current export.
                         stdout carries the private key: pipe it to the updater
                         and never to a log.

Options:
  --allowlist <PATH>            default /etc/ato/cert-export/allowlist.conf
  --instance <ID>              allowlisted Instance to act on (required)
  --domain <DNS>               assert the allowlisted domain (optional)
  --state-root <PATH>          default /var/lib/ato/cert-export
  --export-dir <PATH>          override <state-root>/<instance>
  --openssl <PATH>             default /usr/bin/openssl
  --min-remaining-hours <N>    refuse a pair expiring sooner (default 48)
  --warn-remaining-days <N>    warn when expiring sooner (default 14)
  --max-validity-days <N>      refuse an implausible window (default 400)
  --grant-id <ID>              operator renewal grant (print-binding-update)
  --expected-generation <N>    durable Binding config generation (print-binding-update)
  --allow-unprivileged         TEST ONLY: skip the root and file-ownership checks
  -h, --help                   this text

Exit codes: 0 ok (including unchanged), 2 pair rejected, 3 usage or allowlist,
4 privilege, 5 host tool or export directory failure.
";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ato-cert-export: {error}");
            ExitCode::from(error.exit_code() as u8)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Check,
    Export,
    PrintBindingUpdate,
}

struct Options {
    command: Command,
    allowlist: PathBuf,
    instance: String,
    domain: Option<String>,
    state_root: PathBuf,
    export_dir: Option<PathBuf>,
    openssl: PathBuf,
    thresholds: Thresholds,
    allow_unprivileged: bool,
    print_binding_update: bool,
    grant_id: Option<String>,
    expected_generation: Option<u64>,
}

enum Parsed {
    Help,
    Run(Box<Options>),
}

fn parse(arguments: impl Iterator<Item = String>) -> Result<Parsed> {
    let mut command = None;
    let mut options = Options {
        command: Command::Check,
        allowlist: PathBuf::from(DEFAULT_ALLOWLIST),
        instance: String::new(),
        domain: None,
        state_root: PathBuf::from(DEFAULT_STATE_ROOT),
        export_dir: None,
        openssl: PathBuf::from(DEFAULT_OPENSSL),
        thresholds: Thresholds::default(),
        allow_unprivileged: false,
        print_binding_update: false,
        grant_id: None,
        expected_generation: None,
    };
    let mut arguments = arguments.peekable();
    while let Some(argument) = arguments.next() {
        let (name, inline) = match argument.split_once('=') {
            Some((name, value)) if name.starts_with("--") => {
                (name.to_owned(), Some(value.to_owned()))
            }
            _ => (argument.clone(), None),
        };
        let mut value = || -> Result<String> {
            if let Some(value) = inline.clone() {
                return Ok(value);
            }
            arguments
                .next()
                .ok_or_else(|| Error::usage(format!("{name} needs a value")))
        };
        match name.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--allow-unprivileged" => options.allow_unprivileged = true,
            "--print-binding-update" => options.print_binding_update = true,
            "--allowlist" => options.allowlist = PathBuf::from(value()?),
            "--instance" => options.instance = value()?,
            "--domain" => options.domain = Some(value()?),
            "--state-root" => options.state_root = PathBuf::from(value()?),
            "--export-dir" => options.export_dir = Some(PathBuf::from(value()?)),
            "--openssl" => options.openssl = PathBuf::from(value()?),
            "--min-remaining-hours" => {
                options.thresholds.min_remaining_hours = number(&value()?, &name)?
            }
            "--warn-remaining-days" => {
                options.thresholds.warn_remaining_days = number(&value()?, &name)?
            }
            "--max-validity-days" => {
                options.thresholds.max_validity_days = number(&value()?, &name)?
            }
            "--grant-id" => options.grant_id = Some(value()?),
            "--expected-generation" => {
                options.expected_generation = Some(number::<u64>(&value()?, &name)?);
            }
            "check" | "export" | "print-binding-update" => {
                if command.is_some() {
                    return Err(Error::usage("exactly one command is allowed"));
                }
                command = Some(match name.as_str() {
                    "check" => Command::Check,
                    "export" => Command::Export,
                    _ => Command::PrintBindingUpdate,
                });
            }
            other if other.starts_with('-') => {
                return Err(Error::usage(format!("unknown option {other}")));
            }
            other => return Err(Error::usage(format!("unknown command {other}"))),
        }
    }
    let command = command.ok_or_else(|| Error::usage("a command is required"))?;
    if options.instance.is_empty() {
        return Err(Error::usage("--instance is required"));
    }
    if command == Command::Export && options.print_binding_update {
        options.command = Command::Export;
    }
    options.command = command;
    Ok(Parsed::Run(Box::new(options)))
}

fn number<T: std::str::FromStr>(value: &str, name: &str) -> Result<T> {
    value
        .parse()
        .map_err(|_| Error::usage(format!("{name} needs a number")))
}

fn run() -> Result<()> {
    let options = match parse(std::env::args().skip(1))? {
        Parsed::Help => {
            print!("{USAGE}");
            return Ok(());
        }
        Parsed::Run(options) => *options,
    };
    if options.print_binding_update && options.command != Command::Export {
        return Err(Error::usage(
            "--print-binding-update is only meaningful with export",
        ));
    }
    let privileged = !options.allow_unprivileged;
    ato_cert_export::require_privilege(
        options.allow_unprivileged,
        ato_cert_export::effective_uid(),
    )?;
    let entries = allowlist::load(&options.allowlist, privileged)?;
    let entry = allowlist::select(&entries, &options.instance, options.domain.as_deref())?;
    let inspector = OpensslCli::new(&options.openssl, privileged)?;
    let export_dir = options
        .export_dir
        .clone()
        .unwrap_or_else(|| export::export_dir_for(&options.state_root, entry));
    let now = ato_cert_export::time::now_epoch_secs();

    match options.command {
        Command::Check => {
            let validation = export::validate(entry, options.thresholds, now, &inspector)?;
            let bytes = std::fs::read(&entry.certificate).map_err(Error::io)?;
            let plan = payload::plan(
                entry,
                &export_dir,
                &validation,
                "check",
                &export::sha256_hex(&bytes),
                now,
            );
            report(&plan, false)?;
        }
        Command::Export => {
            let outcome = export::export(entry, options.thresholds, now, &inspector, &export_dir)?;
            let plan = payload::plan(
                entry,
                &export_dir,
                &outcome.validation,
                outcome.action.as_str(),
                &outcome.meta.source.certificate_sha256,
                now,
            );
            // When the payload is requested, stdout belongs to the payload
            // alone: the plan goes to stderr so a pipe into the updater cannot
            // pick it up.
            report(&plan, options.print_binding_update)?;
            if options.print_binding_update {
                print_payload(
                    entry,
                    &export_dir,
                    options.grant_id.as_deref(),
                    options.expected_generation,
                )?;
            }
        }
        Command::PrintBindingUpdate => print_payload(
            entry,
            &export_dir,
            options.grant_id.as_deref(),
            options.expected_generation,
        )?,
    }
    Ok(())
}

/// The journal-safe line: identifiers and certificate facts, never key material.
fn report(plan: &payload::Plan, to_stderr: bool) -> Result<()> {
    let line = serde_json::to_string(plan)?;
    if to_stderr {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
    for warning in &plan.warnings {
        eprintln!("ato-cert-export: warning: {warning}");
    }
    eprintln!(
        "ato-cert-export: {} {} serial={} not_after={} dir={}",
        plan.action, plan.domain, plan.serial, plan.not_after, plan.export_dir
    );
    Ok(())
}

fn print_payload(
    entry: &ato_cert_export::allowlist::Entry,
    export_dir: &std::path::Path,
    grant_id: Option<&str>,
    expected_generation: Option<u64>,
) -> Result<()> {
    let grant_id = grant_id.ok_or_else(|| Error::usage("--grant-id is required"))?;
    let expected_generation =
        expected_generation.ok_or_else(|| Error::usage("--expected-generation is required"))?;
    let exported = export::load_export(export_dir)?;
    if exported.meta.instance_id != entry.instance_id || exported.meta.domain != entry.domain {
        return Err(Error::usage(
            "the export directory belongs to a different (instance, domain) pair",
        ));
    }
    if exported.meta.certificate_binding != entry.certificate_binding
        || exported.meta.private_key_binding != entry.private_key_binding
    {
        return Err(Error::usage(
            "the export directory binds different binding ids than the allowlist",
        ));
    }
    let update = payload::binding_update(entry, &exported, grant_id, expected_generation);
    eprintln!(
        "ato-cert-export: stdout carries the Binding values (including the private key). Pipe it straight into the updater; never write it to a log."
    );
    println!("{}", serde_json::to_string(&update)?);
    Ok(())
}
