use std::path::{Path, PathBuf};

pub fn expand_local_path(raw: &str) -> PathBuf {
    if raw == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(raw));
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

pub fn is_explicit_local_path_input(raw: &str) -> bool {
    if raw.is_empty() {
        return false;
    }
    if raw == "." || raw == ".." {
        return true;
    }
    if raw.starts_with("./")
        || raw.starts_with("../")
        || raw.starts_with(".\\")
        || raw.starts_with("..\\")
        || raw.starts_with("~/")
        || raw.starts_with("~\\")
        || raw.starts_with('/')
        || raw.starts_with('\\')
    {
        return true;
    }

    raw.len() >= 3
        && raw.as_bytes()[1] == b':'
        && (raw.as_bytes()[2] == b'/' || raw.as_bytes()[2] == b'\\')
        && raw.as_bytes()[0].is_ascii_alphabetic()
}

pub fn looks_like_local_capsule_artifact(raw: &str) -> bool {
    let trimmed = raw.trim();
    !trimmed.is_empty() && trimmed.ends_with(".capsule")
}

pub fn should_treat_input_as_local(raw: &str, expanded_path: &Path) -> bool {
    expanded_path.exists()
        || is_explicit_local_path_input(raw)
        || looks_like_local_capsule_artifact(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_local_path_rules() {
        assert!(is_explicit_local_path_input("./foo"));
        assert!(is_explicit_local_path_input("../foo"));
        assert!(is_explicit_local_path_input("~/foo"));
        assert!(is_explicit_local_path_input("/tmp/foo"));
        assert!(is_explicit_local_path_input("."));
        assert!(is_explicit_local_path_input(".."));
        assert!(!is_explicit_local_path_input("foo"));
        assert!(!is_explicit_local_path_input("foo/bar"));
    }

    #[test]
    fn bare_relative_scoped_like_input_is_not_local() {
        let tmp = tempfile::tempdir().unwrap();
        let scoped_like = tmp.path().join("my-org").join("my-tool");
        std::fs::create_dir_all(&scoped_like).unwrap();
        assert!(!is_explicit_local_path_input("my-org/my-tool"));
    }

    #[test]
    fn looks_like_local_capsule_artifact_accepts_bare_capsule_filename() {
        assert!(looks_like_local_capsule_artifact("demo.capsule"));
        assert!(!looks_like_local_capsule_artifact("demo"));
        assert!(!looks_like_local_capsule_artifact("koh0920/demo"));
    }

    #[test]
    fn should_treat_existing_local_input_as_local() {
        let tmp = tempfile::tempdir().unwrap();
        let artifact = tmp.path().join("demo.capsule");
        std::fs::write(&artifact, b"capsule").unwrap();
        assert!(should_treat_input_as_local(
            "demo.capsule",
            Path::new(&artifact)
        ));
    }
}
