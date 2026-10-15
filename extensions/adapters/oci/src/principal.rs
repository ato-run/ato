//! Optional physical evidence, never an admission grant or Capsule identity.

use std::io::Read;
use std::path::Path;

/// Credentials of the container init process at the existing startup inspect.
/// Missing evidence means unknown, including a remote daemon or user remapping.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OciExecutionPrincipal {
    pub source: &'static str,
    pub observed_stage: &'static str,
    pub uid_namespace: &'static str,
    /// Real, effective, saved and filesystem IDs from Linux procfs.
    pub uids: [u32; 4],
    pub gids: [u32; 4],
    pub supplementary_groups: Vec<u32>,
}

pub(crate) fn observe(container_id: &str, pid: &str) -> Option<OciExecutionPrincipal> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let pid: u32 = pid.parse().ok()?;
    if pid == 0 || container_id.len() != 64 || !container_id.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return None;
    }
    from_proc(Path::new("/proc"), container_id, pid)
}

fn bounded(path: &Path) -> Option<String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(8193)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() <= 8192).then_some(text)
}

fn start_time(text: &str) -> Option<u64> {
    // comm may contain spaces and ')'; the last ')' ends field2.
    text.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

fn identity_mapping(text: &str) -> bool {
    text.split_whitespace().eq(["0", "0", "4294967295"])
}

fn from_proc(root: &Path, container_id: &str, pid: u32) -> Option<OciExecutionPrincipal> {
    let root = root.join(pid.to_string());
    let before = start_time(&bounded(&root.join("stat"))?)?;
    let status = bounded(&root.join("status"))?;
    let cgroup = bounded(&root.join("cgroup"))?;
    // A reused PID, remote daemon or unrelated local process is not evidence.
    let scope = format!("docker-{container_id}.scope");
    if !cgroup.lines().any(|line| {
        line.splitn(3, ':').nth(2).is_some_and(|path| {
            path.split('/')
                .any(|part| part == container_id || part == scope)
        })
    }) || !identity_mapping(&bounded(&root.join("uid_map"))?)
        || !identity_mapping(&bounded(&root.join("gid_map"))?)
    {
        return None;
    }
    let value = parse_status(&status)?;
    (start_time(&bounded(&root.join("stat"))?)? == before).then_some(value)
}

fn parse_status(text: &str) -> Option<OciExecutionPrincipal> {
    fn fields(text: &str, name: &str) -> Option<Vec<u32>> {
        let mut lines = text.lines().filter_map(|line| line.strip_prefix(name));
        let line = lines.next()?;
        if lines.next().is_some() {
            return None;
        }
        let values = line
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<u32>, _>>()
            .ok()?;
        (values.len() <= 64).then_some(values)
    }
    Some(OciExecutionPrincipal {
        source: "linux_proc_status",
        observed_stage: "container_start",
        uid_namespace: "runner_host_identity_mapping",
        uids: fields(text, "Uid:")?.try_into().ok()?,
        gids: fields(text, "Gid:")?.try_into().ok()?,
        supplementary_groups: fields(text, "Groups:")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "read-only smoke requires an explicitly selected existing Docker container on Linux"]
    fn selected_existing_container_kernel_reader() {
        let container = std::env::var("ATO_TEST_EXISTING_CONTAINER_ID").unwrap();
        let pid = std::env::var("ATO_TEST_EXISTING_CONTAINER_PID").unwrap();
        let value =
            observe(&container, &pid).expect("this container's unremapped kernel credentials");
        // This is a reader smoke, not evidence of this app's startup or K.
        println!(
            "kernel_reader_only uids={:?} gids={:?} groups={:?}",
            value.uids, value.gids, value.supplementary_groups
        );
    }

    #[test]
    fn status_reports_root_instead_of_claiming_nonroot_and_rejects_incomplete_fields() {
        let valid = "Name:\tmemos\nUid:\t10001 10001 10001 10001\nGid:\t911 911 911 911\nGroups:\t911 10001\n";
        let value = parse_status(valid).unwrap();
        assert_eq!(value.uids, [10001; 4]);
        assert_eq!(value.gids, [911; 4]);
        assert_eq!(value.supplementary_groups, [911, 10001]);
        assert_eq!(
            parse_status(&valid.replace("10001", "0")).unwrap().uids,
            [0; 4]
        );
        for text in [
            valid.replace("Uid:\t10001 10001 10001 10001", "Uid:\t10001"),
            valid.replace("10001", "4294967296"),
            format!("{valid}Uid:\t1 1 1 1\n"),
            valid.replace("Groups:\t911 10001\n", ""),
        ] {
            assert!(parse_status(&text).is_none());
        }
    }

    #[test]
    fn proc_evidence_requires_this_container_and_an_unremapped_namespace() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("123");
        std::fs::create_dir(&root).unwrap();
        let id = "a".repeat(64);
        let stat = format!("123 (name with ) spaces) S {} 12345", ["0"; 18].join(" "));
        assert_eq!(start_time(&stat), Some(12345));
        for (name, text) in [
            ("stat", stat),
            (
                "status",
                "Uid: 10001 10001 10001 10001\nGid: 10001 10001 10001 10001\nGroups:\n".into(),
            ),
            ("cgroup", format!("0::/system.slice/docker-{id}.scope\n")),
            ("uid_map", "         0          0 4294967295\n".into()),
            ("gid_map", "0 0 4294967295\n".into()),
        ] {
            std::fs::write(root.join(name), text).unwrap();
        }
        assert_eq!(
            from_proc(directory.path(), &id, 123).unwrap().uids,
            [10001; 4]
        );
        assert!(from_proc(directory.path(), &"b".repeat(64), 123).is_none());
        std::fs::write(root.join("uid_map"), "0 100000 65536\n").unwrap();
        assert!(from_proc(directory.path(), &id, 123).is_none());
        std::fs::write(root.join("uid_map"), "0 0 4294967295\n").unwrap();
        std::fs::write(root.join("status"), "X".repeat(8193)).unwrap();
        assert!(from_proc(directory.path(), &id, 123).is_none());
    }
}
