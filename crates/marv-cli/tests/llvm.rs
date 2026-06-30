use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "marv-cli-llvm-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).expect("create temp dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn marv() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_marv"));
    cmd.current_dir(repo_root());
    cmd
}

fn run(mut cmd: Command) -> Output {
    let output = cmd.output().expect("run command");
    if !output.status.success() {
        panic!(
            "command failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    output
}

fn clang_available() -> bool {
    Command::new("clang").arg("--version").output().is_ok()
}

#[test]
fn native_llvm_run_and_linked_executable_work() {
    if !clang_available() {
        return;
    }
    let mut run_cmd = marv();
    run_cmd.args([
        "build",
        "--target",
        "native-llvm",
        "--run",
        "examples/factorial.mv",
        "--entry",
        "factorial",
        "6",
    ]);
    let output = run(run_cmd);
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "720");

    let tmp = TempDir::new("factorial");
    let exe = tmp.path().join(if cfg!(windows) {
        "factorial.exe"
    } else {
        "factorial"
    });
    let mut build = marv();
    build
        .args(["build", "--target", "native-llvm", "--out"])
        .arg(&exe)
        .args(["examples/factorial.mv", "--entry", "factorial"]);
    run(build);

    let output = run({
        let mut cmd = Command::new(&exe);
        cmd.arg("6");
        cmd
    });
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "720");
}

#[test]
fn native_llvm_runs_recursive_json_dom_serializer_path() {
    if !clang_available() {
        return;
    }
    let mut run_cmd = marv();
    run_cmd.args([
        "build",
        "--target",
        "native-llvm",
        "--run",
        "tests/run/json_dom.mv",
        "--entry",
        "exercise",
    ]);
    let output = run(run_cmd);
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "379");
}

#[test]
fn native_llvm_reports_reachable_capability_perform() {
    let output = marv()
        .args([
            "build",
            "--target",
            "native-llvm",
            "--run",
            "examples/hello.mv",
            "--entry",
            "main",
        ])
        .output()
        .expect("run marv");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported: capability perform"),
        "stderr should explain unsupported capability perform, got:\n{stderr}"
    );
}

#[test]
fn native_llvm_reports_reachable_resource_capability_perform() {
    let output = marv()
        .args([
            "build",
            "--target",
            "native-llvm",
            "examples/resource_lifecycle.mv",
            "--entry",
            "file_scope",
        ])
        .output()
        .expect("run marv");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported: capability perform"),
        "stderr should explain unsupported resource capability perform, got:\n{stderr}"
    );
}

#[test]
fn native_llvm_reports_reachable_raise() {
    let output = marv()
        .args([
            "build",
            "--target",
            "native-llvm",
            "--run",
            "tests/run/json.mv",
            "--entry",
            "invalid_scalar",
        ])
        .output()
        .expect("run marv");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported: raise"),
        "stderr should explain unsupported raise, got:\n{stderr}"
    );
}

#[test]
fn native_llvm_reports_reachable_unsafe_extern_without_body() {
    let output = marv()
        .args([
            "build",
            "--target",
            "native-llvm",
            "--run",
            "examples/unsafe_audit.mv",
            "--entry",
            "audited_add_one",
            "41",
        ])
        .output()
        .expect("run marv");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported: function without a body"),
        "stderr should explain unsupported unsafe extern call, got:\n{stderr}"
    );
}
