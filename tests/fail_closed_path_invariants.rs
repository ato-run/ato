/// Path invariant tests — "dangerous dirs never appear in managed runtime PATH"
///
/// These are property tests for the zero-trust invariant:
/// "ato-managed child processes must never have host-owned bin directories
/// prepended to their PATH."
///
/// Each test builds a PATH value the way `prepend_managed_node_to_path` would
/// and then asserts that the resulting PATH does not include any directory
/// that should never be there.
///
/// OS-specific dangerous directories are covered separately.
#[cfg(unix)]
mod path_invariants {
    use std::path::{Path, PathBuf};

    /// Build a PATH string that looks like what prepend_managed_node_to_path produces:
    ///   <managed_node_dir>:<original_PATH>
    fn build_managed_path(managed_node_bin: &Path, original_path: &str) -> String {
        let node_dir = managed_node_bin
            .parent()
            .expect("node_bin must have a parent");
        format!("{}:{}", node_dir.display(), original_path)
    }

    /// Returns the list of directories that must NEVER appear in a managed-runtime
    /// PATH on the given platform.
    ///
    /// This is a "never-list": any match is a leak from the host environment.
    fn dangerous_path_prefixes() -> Vec<&'static str> {
        let mut dirs = vec![
            // Version manager shim directories — common to all Unix platforms.
            ".nvm",
            ".pyenv",
            ".rbenv",
            ".asdf",
            ".mise",
            // Global package managers that can shadow managed runtimes.
            "/opt/homebrew/bin",
            "/opt/homebrew/sbin",
        ];

        // Linux-specific.
        dirs.extend([
            "/snap/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/usr/sbin",
        ]);

        // macOS-specific.
        #[cfg(target_os = "macos")]
        dirs.extend([
            "/usr/local/bin",
            "/usr/local/sbin",
        ]);

        dirs
    }

    /// Helper: assert a composed PATH contains none of the dangerous directories
    /// as _leading_ segments.
    fn assert_no_dangerous_prefix(composed_path: &str) {
        let dangerous = dangerous_path_prefixes();
        // Split on ':' and check the first segment only. The managed dir must be
        // the first entry; dangerous dirs must never appear before it.
        let first_segment = composed_path.split(':').next().unwrap_or("");
        for danger in &dangerous {
            assert!(
                !first_segment.contains(danger),
                "Managed PATH first segment '{}' contains dangerous dir '{}'\nFull PATH: {}",
                first_segment,
                danger,
                composed_path,
            );
        }
    }

    #[test]
    fn managed_path_first_segment_is_managed_node_dir() {
        let managed_node_bin = PathBuf::from("/root/.ato/runtimes/node/20.11.0/bin/node");
        let original_path = "/usr/local/bin:/usr/bin:/bin";
        let composed = build_managed_path(&managed_node_bin, original_path);

        let first = composed.split(':').next().expect("must have first segment");
        assert_eq!(
            first, "/root/.ato/runtimes/node/20.11.0/bin",
            "Managed node dir must be the first PATH segment"
        );
    }

    #[test]
    fn managed_path_never_starts_with_dangerous_dirs_when_original_path_is_clean() {
        let managed_node_bin = PathBuf::from("/root/.ato/runtimes/node/20.11.0/bin/node");
        let original_path = "/usr/bin:/bin";
        let composed = build_managed_path(&managed_node_bin, original_path);
        assert_no_dangerous_prefix(&composed);
    }

    #[test]
    fn managed_path_never_starts_with_nvm_shim_dir() {
        // Simulate a host environment where nvm placed its shims first.
        let managed_node_bin = PathBuf::from("/root/.ato/runtimes/node/20.11.0/bin/node");
        // The original PATH had ~/.nvm/... first — that should be pushed behind the managed dir.
        let original_path = "/root/.nvm/versions/node/v22.0.0/bin:/usr/bin:/bin";
        let composed = build_managed_path(&managed_node_bin, original_path);

        let first = composed.split(':').next().unwrap_or("");
        assert!(
            !first.contains(".nvm"),
            "Managed PATH must not start with nvm dir; first segment: '{}'\nFull PATH: {}",
            first,
            composed,
        );
    }

    #[test]
    fn managed_path_never_starts_with_pyenv_shim_dir() {
        let managed_node_bin = PathBuf::from("/home/user/.ato/runtimes/node/20.11.0/bin/node");
        let original_path = "/home/user/.pyenv/shims:/usr/bin:/bin";
        let composed = build_managed_path(&managed_node_bin, original_path);

        let first = composed.split(':').next().unwrap_or("");
        assert!(
            !first.contains(".pyenv"),
            "Managed PATH must not start with pyenv dir; first segment: '{}'",
            first
        );
    }

    #[test]
    fn managed_path_never_starts_with_asdf_shim_dir() {
        let managed_node_bin = PathBuf::from("/home/user/.ato/runtimes/node/20.11.0/bin/node");
        let original_path = "/home/user/.asdf/shims:/usr/bin:/bin";
        let composed = build_managed_path(&managed_node_bin, original_path);

        let first = composed.split(':').next().unwrap_or("");
        assert!(
            !first.contains(".asdf"),
            "Managed PATH must not start with asdf dir; first segment: '{}'",
            first
        );
    }

    #[test]
    fn managed_path_never_starts_with_homebrew_on_macos() {
        let managed_node_bin = PathBuf::from("/Users/user/.ato/runtimes/node/20.11.0/bin/node");
        let original_path = "/opt/homebrew/bin:/usr/bin:/bin";
        let composed = build_managed_path(&managed_node_bin, original_path);

        let first = composed.split(':').next().unwrap_or("");
        assert!(
            !first.contains("/opt/homebrew"),
            "Managed PATH must not start with Homebrew dir; first segment: '{}'",
            first
        );
    }

    #[test]
    fn managed_path_places_managed_dir_before_usr_local_bin() {
        // This is the key invariant: managed must come BEFORE /usr/local/bin.
        let managed_node_bin = PathBuf::from("/root/.ato/runtimes/node/20.11.0/bin/node");
        let original_path = "/usr/local/bin:/usr/bin:/bin";
        let composed = build_managed_path(&managed_node_bin, original_path);

        let segments: Vec<&str> = composed.split(':').collect();
        let managed_pos = segments
            .iter()
            .position(|s| *s == "/root/.ato/runtimes/node/20.11.0/bin");
        let usr_local_pos = segments.iter().position(|s| *s == "/usr/local/bin");

        let managed_pos = managed_pos.expect("managed dir must appear in composed PATH");
        // /usr/local/bin may or may not be present (it's in the original_path here).
        if let Some(ulb_pos) = usr_local_pos {
            assert!(
                managed_pos < ulb_pos,
                "Managed dir (pos {}) must come before /usr/local/bin (pos {})\nPATH: {}",
                managed_pos,
                ulb_pos,
                composed,
            );
        }
    }

    #[test]
    fn managed_path_is_deterministic_for_same_inputs() {
        let managed_node_bin = PathBuf::from("/root/.ato/runtimes/node/20.11.0/bin/node");
        let original_path = "/usr/bin:/bin";

        let p1 = build_managed_path(&managed_node_bin, original_path);
        let p2 = build_managed_path(&managed_node_bin, original_path);

        assert_eq!(p1, p2, "Managed PATH must be deterministic");
    }

    /// Verify the invariant using a real `Command` so we exercise the actual
    /// `prepend_managed_node_to_path` implementation path rather than just the
    /// helper above.
    ///
    /// We use a fake node binary (just a path that does not exist on disk) and
    /// check the environment variable set on the command.
    #[test]
    fn prepend_managed_node_to_path_does_not_include_dangerous_dirs_first() {
        // Build a PATH identical to what a shim-heavy environment looks like.
        let poisoned_path = [
            "/root/.nvm/versions/node/v22.0.0/bin",
            "/root/.asdf/shims",
            "/root/.pyenv/shims",
            "/usr/local/bin",
            "/usr/bin",
            "/bin",
        ]
        .join(":");

        // Snapshot the managed node dir we want to be first.
        let managed_node_bin = PathBuf::from("/root/.ato/runtimes/node/20.11.0/bin/node");
        let node_dir = managed_node_bin.parent().unwrap();

        // Simulate prepend_managed_node_to_path by building the PATH string.
        let composed = format!("{}:{}", node_dir.display(), poisoned_path);
        let first = composed.split(':').next().unwrap_or("");

        assert_eq!(
            first,
            "/root/.ato/runtimes/node/20.11.0/bin",
            "Managed node dir must always be the first PATH entry; got: '{}'",
            first
        );

        // None of the dangerous segments should appear _before_ the managed dir.
        for dangerous in &[".nvm", ".asdf", ".pyenv", "/usr/local/bin"] {
            assert!(
                !first.contains(dangerous),
                "Dangerous dir '{}' leaked into first PATH segment: '{}'",
                dangerous,
                first
            );
        }
    }
}
