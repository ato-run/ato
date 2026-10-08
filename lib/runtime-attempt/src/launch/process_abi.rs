//! Measured process-image ABI admission, before creating the workload.
use anyhow::{Context, Result, ensure};

fn version(value: &str) -> Option<Vec<u16>> {
    if !ato_formation::requirements::valid_abi_version(value) {
        return None;
    }
    let mut parts: Vec<_> = value
        .split('.')
        .map(str::parse::<u16>)
        .collect::<Result<_, _>>()
        .ok()?;
    parts.resize(3, 0);
    Some(parts)
}

pub fn covers(have: &str, need: &str) -> bool {
    matches!((version(have),version(need)), (Some(have), Some(need)) if have >= need)
}

pub fn glibcxx_version(bytes: &[u8]) -> Option<String> {
    bytes
        .split(|b| *b == 0)
        .filter_map(|part| {
            let text = std::str::from_utf8(part).ok()?.strip_prefix("GLIBCXX_")?;
            version(text).map(|parts| (parts, text))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, text)| text.to_owned())
}

pub fn check(glibc_min: &str, glibcxx_min: &str) -> Result<()> {
    ensure!(
        cfg!(target_os = "linux"),
        "process_abi_unknown: requires Linux GNU ABI"
    );
    let output = std::process::Command::new("/usr/bin/getconf")
        .arg("GNU_LIBC_VERSION")
        .output()
        .context("process_abi_unknown: glibc")?;
    ensure!(output.status.success(), "process_abi_unknown: glibc");
    let glibc = std::str::from_utf8(&output.stdout)?
        .trim()
        .strip_prefix("glibc ")
        .context("process_abi_unknown: glibc")?;
    let paths = match std::env::consts::ARCH {
        "x86_64" => [
            "/usr/lib/x86_64-linux-gnu/libstdc++.so.6",
            "/usr/lib64/libstdc++.so.6",
        ],
        "aarch64" => [
            "/usr/lib/aarch64-linux-gnu/libstdc++.so.6",
            "/usr/lib64/libstdc++.so.6",
        ],
        _ => anyhow::bail!("process_abi_unknown: architecture"),
    };
    let glibcxx = paths
        .iter()
        .find_map(|path| {
            let metadata = std::fs::metadata(path).ok()?;
            if metadata.len() > 64 * 1024 * 1024 {
                return None;
            }
            glibcxx_version(&std::fs::read(path).ok()?)
        })
        .context("process_abi_unknown: libstdc++")?;
    ensure!(
        covers(glibc, glibc_min) && covers(&glibcxx, glibcxx_min),
        "process_abi_incompatible: glibc={glibc}, GLIBCXX={glibcxx}; required glibc={glibc_min}, GLIBCXX={glibcxx_min}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn abi_is_numeric_and_unknown_is_never_satisfied() {
        assert!(covers("2.39", "2.38"));
        assert!(!covers("2.35", "2.38"));
        assert!(!covers("3.4.30", "3.4.32"));
        assert!(covers("3.4.33", "3.4.9"));
        assert!(!covers("unknown", "2.38"));
    }
    #[test]
    fn only_version_export_names_are_measured() {
        assert_eq!(
            glibcxx_version(b"\0GLIBCXX_3.4.9\0GLIBCXX_3.4.32\0GLIBCXX_DEBUG_MESSAGE_LENGTH\0"),
            Some("3.4.32".into())
        );
    }
}
