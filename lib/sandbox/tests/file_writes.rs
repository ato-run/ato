#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Command;

use ato_sandbox::{SandboxPolicy, apply_sandbox, set_no_new_privs};

const CHILD: &str = "ATO_FILE_WRITE_TEST_CHILD";
const ROOT: &str = "ATO_FILE_WRITE_TEST_ROOT";

fn child_command(root: &Path, mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "file_write_child", "--nocapture"])
        .env(CHILD, mode)
        .env(ROOT, root);
    command
}

#[test]
fn existing_file_writes_cover_procfs_descendants_without_create_or_remove() {
    let parent = std::env::temp_dir().join("ato-file-write-test");
    std::fs::create_dir_all(&parent).unwrap();
    let root = tempfile::tempdir_in(parent).unwrap();
    std::fs::write(root.path().join("existing"), b"before").unwrap();
    let forbidden = tempfile::tempdir_in(root.path().parent().unwrap()).unwrap();
    std::fs::write(forbidden.path().join("credential"), b"test-only-private").unwrap();
    std::os::unix::fs::symlink(
        forbidden.path().join("credential"),
        root.path().join("escape"),
    )
    .unwrap();
    assert!(
        child_command(root.path(), "allowed")
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        std::fs::read(root.path().join("existing")).unwrap(),
        b"truncate"
    );
    assert!(
        child_command(root.path(), "baseline")
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn file_write_child() {
    let Ok(mode) = std::env::var(CHILD) else {
        return;
    };
    let root = PathBuf::from(std::env::var(ROOT).unwrap());
    if mode == "descendant" {
        rename_thread();
        return;
    }
    let executable = std::env::current_exe().unwrap();
    let mut policy = SandboxPolicy::new().with_network(false).allow_read_only([
        PathBuf::from("/usr"),
        PathBuf::from("/lib"),
        PathBuf::from("/lib64"),
        PathBuf::from("/bin"),
        PathBuf::from("/etc"),
        PathBuf::from("/proc"),
        root.clone(),
        executable.parent().unwrap().to_path_buf(),
    ]);
    if mode == "allowed" {
        policy = policy.allow_file_write([root.clone(), PathBuf::from("/proc")]);
    }
    set_no_new_privs().unwrap();
    assert!(apply_sandbox(&policy).unwrap().fully_enforced);
    if mode == "baseline" {
        assert!(
            std::fs::OpenOptions::new()
                .write(true)
                .open(root.join("existing"))
                .is_err()
        );
        assert!(
            std::fs::OpenOptions::new()
                .write(true)
                .open("/proc/self/comm")
                .is_err()
        );
        return;
    }
    use std::io::Write;
    std::fs::OpenOptions::new()
        .write(true)
        .open(root.join("existing"))
        .unwrap()
        .write_all(b"after")
        .unwrap();
    assert!(std::fs::write(root.join("created"), b"new").is_err());
    std::fs::write(root.join("existing"), b"truncate").unwrap();
    assert!(std::fs::remove_file(root.join("existing")).is_err());
    assert!(std::fs::rename(root.join("existing"), root.join("renamed")).is_err());
    assert!(std::fs::read(root.join("escape")).is_err());
    // A procfs magic link must not bypass the denied target's Landlock rule.
    let via_proc = Path::new("/proc/self/root")
        .join(root.strip_prefix("/").unwrap())
        .join("escape");
    assert!(std::fs::read(via_proc).is_err());
    assert!(std::net::TcpListener::bind("127.0.0.1:0").is_err());
    rename_thread();
    assert!(
        child_command(&root, "descendant")
            .status()
            .unwrap()
            .success()
    );
}

fn rename_thread() {
    let (send, receive) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        send.send(std::fs::read_link("/proc/thread-self").unwrap())
            .unwrap();
        wait.recv().unwrap();
    });
    let comm = Path::new("/proc")
        .join(receive.recv().unwrap())
        .join("comm");
    // Match CUDA's O_WRONLY|O_CREAT|O_TRUNC open. procfs comm does not
    // needs the separate Truncate right on ABI 3 and later.
    std::fs::write(&comm, b"ato-file-write").unwrap();
    assert_eq!(std::fs::read_to_string(comm).unwrap(), "ato-file-write\n");
    release.send(()).unwrap();
    thread.join().unwrap();
}
