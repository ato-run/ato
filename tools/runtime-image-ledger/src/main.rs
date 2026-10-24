//! `ato-runtime-image-ledger` — validate and report declared runtime image
//! inventory evidence.
//!
//! Exit codes: 0 valid, 1 findings or an unparseable inventory, 2 usage error
//! or unreadable input.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};

use ato_runtime_image_ledger::model::Inventory;
use ato_runtime_image_ledger::rfc3339;
use ato_runtime_image_ledger::validate::validate_inventory;
use ato_runtime_image_ledger::{report, sha256_digest};

const USAGE: &str = "\
Usage:
  ato-runtime-image-ledger validate <inventory.json> [--quiet]
  ato-runtime-image-ledger report <inventory.json> [--json] [--quiet]

Exit codes:
  0  the inventory is valid
  1  findings are present, or the inventory cannot be parsed
  2  usage error, or the inventory file cannot be read

This tool validates DECLARED evidence only. It does not scan, sign, or verify
images, and it is not the Contract K authority (lib/formation/src/receipt.rs).";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Validate,
    Report,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Options {
    command: Command,
    path: PathBuf,
    quiet: bool,
    json: bool,
}

enum ParseOutcome {
    Help,
    Run(Options),
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let options = match parse_arguments(&arguments) {
        Ok(ParseOutcome::Help) => {
            println!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        Ok(ParseOutcome::Run(options)) => options,
        Err(message) => {
            eprintln!("usage error: {message}");
            eprintln!();
            eprintln!("{USAGE}");
            return Ok(ExitCode::from(2));
        }
    };

    let path = options.path.display().to_string();
    let bytes =
        std::fs::read(&options.path).with_context(|| format!("cannot read inventory {path}"))?;
    let inventory_sha256 = sha256_digest(&bytes);
    let inventory: Inventory = match serde_json::from_slice(&bytes) {
        Ok(inventory) => inventory,
        Err(error) => {
            eprintln!("inventory_json_invalid: inventory: document: {error}");
            return Ok(ExitCode::from(1));
        }
    };

    let findings = validate_inventory(&inventory, rfc3339::now());

    match options.command {
        Command::Validate => {
            for finding in &findings {
                eprintln!("{finding}");
            }
            if findings.is_empty() {
                if !options.quiet {
                    println!(
                        "valid: {} image(s), {} patch candidate(s) [{}]",
                        inventory.images.len(),
                        inventory.patch_candidates.len(),
                        inventory.schema
                    );
                }
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::from(1))
            }
        }
        Command::Report => {
            if !options.quiet {
                if options.json {
                    print!(
                        "{}",
                        report::render_json(&inventory, &findings, &path, &inventory_sha256)?
                    );
                } else {
                    print!(
                        "{}",
                        report::render_text(&inventory, &findings, &path, &inventory_sha256)
                    );
                }
            }
            for finding in &findings {
                eprintln!("{finding}");
            }
            if findings.is_empty() {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::from(1))
            }
        }
    }
}

fn parse_arguments(arguments: &[String]) -> Result<ParseOutcome, String> {
    if arguments.is_empty() {
        return Err("missing subcommand (validate or report)".to_owned());
    }
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help" | "help"))
    {
        return Ok(ParseOutcome::Help);
    }

    let command = match arguments[0].as_str() {
        "validate" => Command::Validate,
        "report" => Command::Report,
        other => {
            return Err(format!(
                "unknown subcommand {other:?}; expected \"validate\" or \"report\""
            ));
        }
    };

    let mut path: Option<PathBuf> = None;
    let mut quiet = false;
    let mut json = false;
    for argument in &arguments[1..] {
        match argument.as_str() {
            "--quiet" | "-q" => quiet = true,
            "--json" => json = true,
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other:?}"));
            }
            other => {
                if path.replace(PathBuf::from(other)).is_some() {
                    return Err(format!("unexpected extra argument {other:?}"));
                }
            }
        }
    }

    if json && command == Command::Validate {
        return Err("--json is only supported by the report subcommand".to_owned());
    }
    if json && quiet {
        return Err("--json cannot be combined with --quiet".to_owned());
    }
    let path = path.ok_or_else(|| "missing <inventory.json> path".to_owned())?;

    Ok(ParseOutcome::Run(Options {
        command,
        path,
        quiet,
        json,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn options(values: &[&str]) -> Result<Options, String> {
        match parse_arguments(&args(values))? {
            ParseOutcome::Run(options) => Ok(options),
            ParseOutcome::Help => Err("unexpected help".to_owned()),
        }
    }

    #[test]
    fn parses_both_subcommands() {
        assert_eq!(
            options(&["validate", "a.json"]).expect("validate").command,
            Command::Validate
        );
        assert_eq!(
            options(&["report", "a.json"]).expect("report").command,
            Command::Report
        );
        assert!(options(&["report", "a.json", "--json"]).expect("json").json);
        assert!(
            options(&["validate", "a.json", "--quiet"])
                .expect("quiet")
                .quiet
        );
    }

    #[test]
    fn rejects_usage_errors() {
        assert!(parse_arguments(&args(&[])).is_err());
        assert!(parse_arguments(&args(&["nope", "a.json"])).is_err());
        assert!(parse_arguments(&args(&["validate"])).is_err());
        assert!(parse_arguments(&args(&["validate", "a.json", "b.json"])).is_err());
        assert!(parse_arguments(&args(&["validate", "a.json", "--bogus"])).is_err());
        assert!(parse_arguments(&args(&["validate", "a.json", "--json"])).is_err());
        assert!(parse_arguments(&args(&["report", "a.json", "--json", "--quiet"])).is_err());
    }

    #[test]
    fn help_is_recognised_anywhere() {
        assert!(matches!(
            parse_arguments(&args(&["--help"])),
            Ok(ParseOutcome::Help)
        ));
        assert!(matches!(
            parse_arguments(&args(&["validate", "a.json", "-h"])),
            Ok(ParseOutcome::Help)
        ));
    }
}
