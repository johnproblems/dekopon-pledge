//! Credential-hygiene guard: the crates.io token file must stay out of git.
//!
//! `.crates.env` holds a live API key. These tests lock in that it is
//! git-ignored and untracked, and prove the guard detects violations using
//! throwaway fixture repos. The file itself is never read, so the key value
//! can never be echoed. Deleting the live file and rotating the key remain
//! human steps on crates.io.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SECRET_FILE: &str = ".crates.env";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Returns `Ok` when [`SECRET_FILE`] is untracked and git-ignored in `dir`.
fn guard_secret_out_of_git(dir: &Path) -> Result<(), String> {
    let tracked = Command::new("git")
        .args(["ls-files", "--", SECRET_FILE])
        .current_dir(dir)
        .output()
        .map_err(|err| format!("git ls-files failed: {err}"))?;
    if !tracked.status.success() {
        return Err(format!("git ls-files exited {}", tracked.status));
    }
    if !String::from_utf8_lossy(&tracked.stdout).trim().is_empty() {
        return Err(format!("{SECRET_FILE} is tracked by git"));
    }
    let ignored = Command::new("git")
        .args(["check-ignore", "-q", SECRET_FILE])
        .current_dir(dir)
        .status()
        .map_err(|err| format!("git check-ignore failed: {err}"))?;
    if !ignored.success() {
        return Err(format!("{SECRET_FILE} is not ignored by git"));
    }
    Ok(())
}

#[test]
fn live_repo_keeps_secret_out_of_git() {
    guard_secret_out_of_git(&repo_root()).expect("live repo must ignore the secret file");
}

#[test]
fn guard_catches_fixture_violations() {
    let scratch = repo_root()
        .join("target")
        .join("tmp")
        .join(format!("secrets-fixture-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);

    // Case 1: tracked secret must fail the guard.
    let tracked_repo = scratch.join("tracked");
    init_repo(&tracked_repo);
    fs::write(tracked_repo.join(SECRET_FILE), "dummy-not-a-key\n").expect("write fixture secret");
    stage(&tracked_repo, SECRET_FILE);
    let err = guard_secret_out_of_git(&tracked_repo).expect_err("tracked secret must fail");
    assert!(err.contains("tracked"), "unexpected message: {err}");

    // Case 2: untracked but un-ignored secret must fail the guard.
    let bare_repo = scratch.join("bare");
    init_repo(&bare_repo);
    fs::write(bare_repo.join(SECRET_FILE), "dummy-not-a-key\n").expect("write fixture secret");
    let err = guard_secret_out_of_git(&bare_repo).expect_err("un-ignored secret must fail");
    assert!(err.contains("not ignored"), "unexpected message: {err}");

    // Case 3: ignored and untracked secret must pass the guard.
    let clean_repo = scratch.join("clean");
    init_repo(&clean_repo);
    fs::write(clean_repo.join(".gitignore"), ".crates.env\n").expect("write fixture ignore");
    fs::write(clean_repo.join(SECRET_FILE), "dummy-not-a-key\n").expect("write fixture secret");
    guard_secret_out_of_git(&clean_repo).expect("ignored secret must pass");

    let _ = fs::remove_dir_all(&scratch);
}

fn init_repo(dir: &Path) {
    fs::create_dir_all(dir).expect("create fixture repo");
    let status = Command::new("git")
        .args(["-c", "init.defaultBranch=main", "init", "-q"])
        .current_dir(dir)
        .status()
        .expect("git init runs");
    assert!(status.success(), "git init failed in fixture");
}

fn stage(dir: &Path, file: &str) {
    let status = Command::new("git")
        .args(["add", "--", file])
        .current_dir(dir)
        .status()
        .expect("git add runs");
    assert!(status.success(), "git add failed in fixture");
}
