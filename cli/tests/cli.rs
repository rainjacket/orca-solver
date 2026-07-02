//! End-to-end smoke tests for the `orca` binary, run against the committed
//! sample grids and `dictionaries/test_small.dict`. Each invocation uses a
//! temporary working directory so generated solution-browser files never
//! land in the repository.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn grid(name: &str) -> String {
    repo_root().join("grids").join(name).display().to_string()
}

fn dict() -> String {
    repo_root()
        .join("dictionaries/test_small.dict")
        .display()
        .to_string()
}

fn orca(args: &[&str]) -> Output {
    let cwd = std::env::temp_dir().join(format!("orca_cli_test_{}", std::process::id()));
    std::fs::create_dir_all(&cwd).expect("create temp cwd");
    Command::new(env!("CARGO_BIN_EXE_orca"))
        .args(args)
        .current_dir(&cwd)
        .output()
        .expect("run orca")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn fill_finds_the_pinned_solution_count() {
    let out = orca(&["fill", &grid("small_3x3.grid"), &dict()]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--- Solution 4 ---"),
        "expected 4 solutions"
    );
    assert!(!stdout.contains("--- Solution 5 ---"), "expected exactly 4");
    assert!(stderr(&out).contains("Total solutions: 4"));
}

#[test]
fn unsatisfiable_grid_reports_zero_solutions() {
    let out = orca(&["fill", &grid("unsatisfiable.grid"), &dict()]);
    assert!(out.status.success());
    assert!(stderr(&out).contains("Total solutions: 0"));
}

#[test]
fn parallel_matches_sequential() {
    let seq = orca(&["fill", &grid("example.grid"), &dict(), "-j", "1"]);
    let par = orca(&["fill", &grid("example.grid"), &dict(), "-j", "2"]);
    assert!(seq.status.success() && par.status.success());
    assert!(stderr(&seq).contains("Total solutions: 2"));
    assert!(stderr(&par).contains("Total solutions: 2"));
}

#[test]
fn missing_grid_file_fails_with_path_in_message() {
    let out = orca(&["fill", "definitely_missing.grid", &dict()]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(
        err.contains("definitely_missing.grid"),
        "error should name the missing file, got: {err}"
    );
}
