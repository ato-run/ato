//! Bounded owner-written execution facts outside every guest-visible path.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::Path,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionFact {
    pub stage: String,
    pub step: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub log_tail: String,
}

pub fn append(
    path: &Path,
    stage: &str,
    step: &str,
    status: Option<std::process::ExitStatus>,
    log: &str,
) -> Result<()> {
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    let mut log_tail = String::new();
    for line in log
        .lines()
        .rev()
        .take(32)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let lower = line.to_ascii_lowercase();
        if [
            "authorization",
            "bearer ",
            "password",
            "token=",
            "secret",
            "credential",
        ]
        .iter()
        .any(|s| lower.contains(s))
        {
            log_tail.push_str("<redacted-sensitive-log>\n");
            continue;
        }
        for word in line.split_whitespace() {
            if word.starts_with("https://") || word.starts_with("http://") {
                log_tail.push_str(&ato_formation::source::redact_url(word));
            } else {
                log_tail.push_str(word);
            }
            log_tail.push(' ');
        }
        log_tail.push('\n');
    }
    while log_tail.len() > 2048 {
        log_tail.remove(0);
    }
    let fact = ExecutionFact {
        stage: stage.into(),
        step: crate::text::bounded_reason(step),
        exit_code: status.and_then(|s| s.code()),
        #[cfg(unix)]
        signal: status.and_then(|s| s.signal()),
        #[cfg(not(unix))]
        signal: None,
        log_tail,
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    ensure!(file.metadata()?.len() < 64 * 1024, "execution facts full");
    writeln!(file, "{}", serde_json::to_string(&fact)?)?;
    file.sync_all()?;
    Ok(())
}

pub fn read(path: &Path) -> Result<Vec<ExecutionFact>> {
    if !path.exists() {
        return Ok(vec![]);
    }
    let mut text = String::new();
    std::fs::File::open(path)?
        .take(64 * 1024 + 1)
        .read_to_string(&mut text)?;
    ensure!(
        text.len() <= 64 * 1024 && text.ends_with('\n'),
        "execution facts corrupt"
    );
    let facts: Vec<_> = text
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    Ok(facts
        .into_iter()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect())
}
