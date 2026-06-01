use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn temp_test_dir(name: &str) -> PathBuf {
    let unique = format!(
        "herdr-workspace-tests-{}-{}-{}",
        name,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);
    std::fs::create_dir_all(&path).unwrap();
    path
}

pub(super) fn write_fake_tracked_repo(root: &Path) {
    let head_oid = "1111111111111111111111111111111111111111";
    let upstream_oid = "2222222222222222222222222222222222222222";
    std::fs::create_dir_all(root.join(".git/refs/heads")).unwrap();
    std::fs::create_dir_all(root.join(".git/refs/remotes/origin")).unwrap();
    std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(root.join(".git/refs/heads/main"), format!("{head_oid}\n")).unwrap();
    std::fs::write(
        root.join(".git/refs/remotes/origin/main"),
        format!("{upstream_oid}\n"),
    )
    .unwrap();
    std::fs::write(
        root.join(".git/config"),
        "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
    )
    .unwrap();
}

pub(super) fn run_git(cwd: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}
